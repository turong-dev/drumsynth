#!/usr/bin/env python3
"""Closed-loop cycle-bench harness for the Teensy 4.1 drum engine.

One command: build the bench, flash it, capture its report over USB serial,
parse it, and diff the peak cycle counts against a saved baseline.

    tools/benchloop.py --label baseline
    tools/benchloop.py --label target-cpu-m7 --baseline baseline

No button press is needed after the first flash. The bench is built with
`--features autoboot`, which makes it run one scenario sweep, print
`=== BENCH END ===`, and then execute `bkpt #251` — the Teensy 4's MKL02
bootloader chip watches for that and takes over. `teensy_loader_cli -w` is
already waiting when it does.

The very first flash is the exception: whatever is on the board now does not
self-reboot, so press the button once when prompted. After that the loop is
hands-free for as long as you keep flashing autoboot benches.

Deliberately standard library only. `pyserial` is not installed and should not
become a prerequisite -- a CDC port on macOS is a character device plus
`tty.setraw`.
"""

import argparse
import glob
import json
import os
import re
import select
import subprocess
import sys
import termios
import time
import tty
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
FIRMWARE = REPO / "firmware"
TARGET_DIR = FIRMWARE / "target" / "thumbv7em-none-eabihf" / "release"
ELF = TARGET_DIR / "bench"
HEX = TARGET_DIR / "bench.hex"
RESULTS = REPO / "bench-results"

SENTINEL = "=== BENCH END ==="
# First line the bench prints. If this is missing from a capture, the host
# opened the port too late and the early scenarios were lost -- macOS discards
# CDC bytes that arrive before the character device is opened.
HEADER = "drum-engine cycle bench"
PORT_GLOB = "/dev/cu.usbmodem*"
MCU = "TEENSY41"

# imxrt-log's `log` frontend writes "[{level} {target}]: {message}\r\n".
PREFIX_RE = re.compile(r"^\[[A-Z]+\s+[^\]]*\]:\s*")

# "8+FX+SWFX  avg= 288611 cy  peak= 288644 cy   72.2% of budget  (9020 cy/frame)"
LINE_RE = re.compile(
    r"^(?P<label>.*?)\s*"
    r"avg=\s*(?P<avg>\d+)\s*cy\s+"
    r"peak=\s*(?P<peak>\d+)\s*cy\s+"
    r"(?P<pct>[\d.]+)%\s*of budget\s*"
    r"\(\s*(?P<frame>\d+)\s*cy/frame\)"
)
BUDGET_RE = re.compile(r"budget\s+(\d+)\s+cycles per block")

# Which linker region each section lands in, per the BSP's generated t4link.x.
# Used to turn `rust-size -A` into a memory-pressure summary, because a change
# that buys cycles by spending ITCM or DTCM it does not have is not a win.
REGIONS = {
    "ITCM": ([".text"], 192 * 1024),
    "DTCM": ([".stack", ".vector_table", ".data", ".bss"], 320 * 1024),
    "OCRAM": ([".rodata", ".uninit", ".heap"], 512 * 1024),
    "FLASH": ([".boot"], 1984 * 1024),
}


class BenchError(Exception):
    """Anything that should stop the run with a readable message."""


class Incomplete(BenchError):
    """A capture that is worth simply retrying.

    Re-flashing is cheap and needs no intervention: the board self-reboots
    into HalfKay at the end of every sweep, so another attempt costs one build
    -- and the build is already cached.
    """

    def __init__(self, message, text=""):
        BenchError.__init__(self, message)
        self.text = text


def run(cmd, cwd=None, capture=False):
    """Run a command, raising BenchError with its output on failure."""
    proc = subprocess.run(
        cmd,
        cwd=str(cwd) if cwd else None,
        stdout=subprocess.PIPE if capture else None,
        stderr=subprocess.STDOUT if capture else None,
        universal_newlines=True,
    )
    if proc.returncode != 0:
        out = proc.stdout or "(output went to the terminal above)"
        raise BenchError("%s failed (exit %d)\n%s" % (" ".join(cmd), proc.returncode, out))
    return proc.stdout or ""


def build(features):
    print("==> building bench (%s)" % features)
    run(
        ["cargo", "build", "--release", "--bin", "bench", "--features", features],
        cwd=FIRMWARE,
    )
    print("==> converting to HEX")
    run(["rust-objcopy", "-O", "ihex", str(ELF), str(HEX)])


def read_sizes():
    """Summarise `rust-size -A` into per-region byte totals."""
    out = run(["rust-size", "-A", str(ELF)], capture=True)
    sections = {}
    for line in out.splitlines():
        parts = line.split()
        if len(parts) >= 2 and parts[0].startswith("."):
            try:
                sections[parts[0]] = int(parts[1])
            except ValueError:
                continue
    regions = {}
    for name, (members, cap) in REGIONS.items():
        used = sum(sections.get(s, 0) for s in members)
        regions[name] = {"used": used, "capacity": cap}
    return {"sections": sections, "regions": regions}


def ports():
    return sorted(glob.glob(PORT_GLOB))


def flash(wait_timeout):
    """Flash the hex, waiting for the board to present itself as HalfKay.

    `-w` blocks until the bootloader appears. An autoboot bench gets there on
    its own within a few seconds of finishing its sweep; anything else needs
    the button.
    """
    before = ports()
    if before:
        print("==> board is running (%s); it will reboot itself at the end of its sweep"
              % ", ".join(before))
    else:
        print("==> no serial port visible -- board may already be in HalfKay, "
              "or needs its button pressed")
    print("==> flashing (press the button on the Teensy if this waits more than ~%ds)"
          % wait_timeout)
    proc = subprocess.run(
        ["teensy_loader_cli", "--mcu=" + MCU, "-w", "-v", str(HEX)],
        timeout=wait_timeout,
    )
    if proc.returncode != 0:
        raise BenchError("teensy_loader_cli failed (exit %d)" % proc.returncode)


def wait_for_port(timeout, exclude=()):
    """Poll hard for the CDC port to (re)appear after the board reboots.

    This is a race against the firmware: the bench waits ~3 s after USB init
    before printing its header, and anything it writes before the host opens
    the character device is dropped by the macOS CDC driver, not buffered. So
    poll at 20 ms and open immediately -- the settle is handled by retrying
    the open rather than by sleeping through the margin.
    """
    deadline = time.time() + timeout
    while time.time() < deadline:
        found = [p for p in ports() if p not in exclude]
        if found:
            return found[0]
        time.sleep(0.02)
    raise BenchError(
        "no %s appeared within %ds. If the board is stuck in HalfKay, re-run; "
        "if it hard-faulted on the bkpt, the LED will be blinking a panic "
        "pattern and you need the button." % (PORT_GLOB, timeout)
    )


def capture(port, timeout):
    """Read raw bytes from the CDC port until the sentinel or a timeout.

    The device vanishes mid-read when it reboots into HalfKay, so an I/O error
    *after* the sentinel is the expected ending, not a failure.
    """
    print("==> capturing from %s" % port)
    # The node can exist a moment before the endpoint accepts an open; retry
    # rather than sleeping a fixed margin, so we start reading as early as
    # possible.
    fd = None
    open_deadline = time.time() + 5.0
    while fd is None:
        try:
            fd = os.open(port, os.O_RDONLY | os.O_NOCTTY | os.O_NONBLOCK)
        except OSError:
            if time.time() > open_deadline:
                raise BenchError("could not open %s within 5s" % port)
            time.sleep(0.01)
    try:
        try:
            tty.setraw(fd)
        except termios.error:
            # Some CDC nodes reject termios setup; reading still works.
            pass
        chunks = []
        deadline = time.time() + timeout
        while time.time() < deadline:
            ready, _, _ = select.select([fd], [], [], 0.5)
            if not ready:
                continue
            try:
                data = os.read(fd, 4096)
            except OSError:
                break  # device went away -- expected once it reboots
            if not data:
                continue
            chunks.append(data)
            if SENTINEL.encode() in b"".join(chunks):
                break
        text = b"".join(chunks).decode("utf-8", errors="replace")
    finally:
        os.close(fd)

    if SENTINEL in text and HEADER not in text:
        raise Incomplete(
            "captured a complete sweep but missed the header, so the early "
            "scenarios were dropped before the port was open", text)

    if SENTINEL not in text:
        rows = len(LINE_RE.findall(PREFIX_RE.sub("", text)))
        if not text:
            hint = ("Nothing arrived at all. The board enumerated but never "
                    "wrote: check it is running the bench and not another binary.")
        elif rows:
            hint = ("Got %d scenario rows, then the board went quiet while still "
                    "enumerated. That is what `teensy4-panic` looks like from the "
                    "host -- it blinks the LED S.O.S. forever and never resets, so "
                    "the port stays up and silent. Look at the LED: a repeating "
                    "3-short/3-long/3-short means the bench panicked in the "
                    "scenario after the last one printed, and the board needs its "
                    "button pressed to get back to HalfKay." % rows)
        else:
            hint = "Output arrived but no scenario rows parsed; the format may have changed."
        raise BenchError(
            "never saw %r within %ds. %s\n\nCaptured %d bytes:\n%s"
            % (SENTINEL, timeout, hint, len(text), text[-2000:] or "(nothing)")
        )
    return text


def parse(text):
    """Pull the budget and the per-scenario rows out of a captured report."""
    budget = None
    scenarios = {}
    order = []
    for raw in text.replace("\r", "\n").splitlines():
        line = PREFIX_RE.sub("", raw).strip()
        if not line:
            continue
        m = BUDGET_RE.search(line)
        if m:
            budget = int(m.group(1))
            continue
        m = LINE_RE.match(line)
        if not m:
            continue
        label = " ".join(m.group("label").split())
        if not label:
            continue
        if label not in scenarios:
            order.append(label)
        scenarios[label] = {
            "avg": int(m.group("avg")),
            "peak": int(m.group("peak")),
            "pct": float(m.group("pct")),
            "cy_per_frame": int(m.group("frame")),
        }
    if not scenarios:
        raise BenchError("captured output contained no scenario rows:\n%s" % text[-2000:])
    return {"budget": budget, "order": order, "scenarios": scenarios}


def load(name):
    path = name if os.path.sep in str(name) else RESULTS / ("%s.json" % name)
    path = Path(path)
    if not path.exists():
        raise BenchError("no baseline at %s" % path)
    with open(str(path)) as fh:
        return json.load(fh)


def fmt_delta(cur, base):
    if base in (None, 0):
        return ""
    d = cur - base
    pct = 100.0 * d / base
    sign = "+" if d > 0 else ""
    return "%s%d (%s%.1f%%)" % (sign, d, sign, pct)


def report(result, baseline):
    budget = result.get("budget")
    print("")
    print("scenario        peak cy   %% budget   %s" % ("delta vs baseline" if baseline else ""))
    print("-" * 72)
    base_sc = (baseline or {}).get("scenarios", {})
    worst = None
    for label in result["order"]:
        row = result["scenarios"][label]
        delta = fmt_delta(row["peak"], base_sc.get(label, {}).get("peak"))
        print("%-14s %9d   %6.1f%%   %s" % (label, row["peak"], row["pct"], delta))
        if worst is None or row["pct"] > worst[1]:
            worst = (label, row["pct"])
    print("-" * 72)
    if budget:
        print("budget %d cycles/block" % budget)
    if worst:
        flag = "  <-- OVER 70%" if worst[1] > 70.0 else ""
        print("worst case: %s at %.1f%% of budget%s" % (worst[0], worst[1], flag))

    sizes = result.get("sizes")
    base_sizes = (baseline or {}).get("sizes")
    if sizes:
        print("")
        print("region      used      cap    %%    %s" % ("delta" if base_sizes else ""))
        print("-" * 72)
        for name in ("ITCM", "DTCM", "OCRAM", "FLASH"):
            r = sizes["regions"][name]
            delta = ""
            if base_sizes:
                delta = fmt_delta(r["used"], base_sizes["regions"][name]["used"])
            print("%-8s %8d %8d %5.1f%%   %s"
                  % (name, r["used"], r["capacity"],
                     100.0 * r["used"] / r["capacity"], delta))


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--label", required=True,
                    help="name for this run; written to bench-results/<label>.json")
    ap.add_argument("--baseline", default=None,
                    help="label (or path) of a previous run to diff against")
    ap.add_argument("--features", default="autoboot",
                    help="cargo features for the bench build (default: autoboot)")
    ap.add_argument("--no-build", action="store_true",
                    help="reuse the existing bench.hex")
    ap.add_argument("--flash-timeout", type=int, default=60,
                    help="seconds to wait for HalfKay (default: 60)")
    ap.add_argument("--port-timeout", type=int, default=30,
                    help="seconds to wait for the CDC port to reappear (default: 30)")
    ap.add_argument("--read-timeout", type=int, default=90,
                    help="seconds to wait for the sentinel (default: 90)")
    ap.add_argument("--retries", type=int, default=3,
                    help="re-flash and re-capture this many times if the capture "
                         "misses the header (default: 3)")
    ap.add_argument("--raw", default=None,
                    help="also write the raw captured serial text here")
    args = ap.parse_args()

    if "autoboot" not in args.features.split(","):
        print("warning: --features %r has no `autoboot`; the bench will free-run "
              "and this harness will time out waiting for the sentinel."
              % args.features, file=sys.stderr)

    if not args.no_build:
        build(args.features)
    elif not HEX.exists():
        raise BenchError("--no-build given but %s does not exist" % HEX)

    sizes = read_sizes()
    before = ports()

    # Racing the firmware's ~3 s pre-header delay is not always winnable on a
    # busy machine, and a capture that misses the header has silently lost its
    # first scenarios. Retrying is cheap and unattended: the board is back in
    # HalfKay the moment the sweep ends.
    text = None
    for attempt in range(1, args.retries + 2):
        flash(args.flash_timeout)
        port = wait_for_port(args.port_timeout, exclude=())
        try:
            text = capture(port, args.read_timeout)
            break
        except Incomplete as exc:
            if attempt > args.retries:
                raise BenchError(
                    "%s (after %d attempts)" % (exc, attempt))
            print("==> incomplete capture (%s); retrying %d/%d"
                  % (exc, attempt, args.retries), file=sys.stderr)

    if args.raw:
        with open(args.raw, "w") as fh:
            fh.write(text)

    result = parse(text)
    result["label"] = args.label
    result["sizes"] = sizes
    result["ports_before"] = before

    RESULTS.mkdir(exist_ok=True)
    out = RESULTS / ("%s.json" % args.label)
    with open(str(out), "w") as fh:
        json.dump(result, fh, indent=2, sort_keys=True)

    baseline = load(args.baseline) if args.baseline else None
    if baseline:
        missing = [k for k in baseline.get("order", []) if k not in result["scenarios"]]
        if missing:
            print("warning: this run is missing %d scenario(s) the baseline has: %s"
                  % (len(missing), ", ".join(missing)), file=sys.stderr)
    report(result, baseline)
    print("")
    print("wrote %s" % out)


if __name__ == "__main__":
    try:
        main()
    except BenchError as exc:
        print("error: %s" % exc, file=sys.stderr)
        sys.exit(1)
    except subprocess.TimeoutExpired:
        print("error: teensy_loader_cli timed out waiting for the bootloader. "
              "Press the button on the Teensy and re-run.", file=sys.stderr)
        sys.exit(1)
    except KeyboardInterrupt:
        sys.exit(130)
