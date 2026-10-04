#!/usr/bin/env python3
"""Change the provisioned X11 desktop mode with a detached rollback watchdog."""
import argparse
import os
from pathlib import Path
import re
import select
import subprocess
import sys
import tempfile
import time


def parse_outputs(text):
    outputs = {}
    output = None
    for line in text.splitlines():
        header = re.match(r"^(\S+) (connected|disconnected)\b", line)
        if header:
            output = header[1] if header[2] == "connected" else None
            if output:
                outputs[output] = {"modes": [], "current": None}
            continue
        mode = re.match(r"^\s+(\d+x\d+)\s+(.+)$", line)
        if output and mode:
            for token in mode[2].split():
                rate = re.fullmatch(r"(\d+(?:\.\d+)?)([+*]*)", token)
                if rate:
                    item = (mode[1], rate[1])
                    outputs[output]["modes"].append(item)
                    if "*" in rate[2]:
                        outputs[output]["current"] = item
    return outputs


def query():
    result = subprocess.run(["xrandr", "--query"], check=True, capture_output=True, text=True)
    return parse_outputs(result.stdout)


def apply(output, mode, rate):
    subprocess.run(["xrandr", "--output", output, "--mode", mode, "--rate", rate], check=True)


def restore(token, output, mode, rate):
    if Path(token).exists():
        apply(output, mode, rate)
        Path(token).unlink(missing_ok=True)


def confirm(timeout=15):
    print(f"Keep this mode? Type yes within {timeout} seconds, otherwise it will revert.", flush=True)
    ready, _, _ = select.select([sys.stdin], [], [], timeout)
    return bool(ready) and sys.stdin.readline().strip().lower() in {"yes", "y"}


def change(output, mode, rate, outputs):
    if output not in outputs or (mode, rate) not in outputs[output]["modes"]:
        raise ValueError("Choose a supported output, resolution and refresh rate from --list.")
    previous = outputs[output]["current"]
    if previous is None:
        raise ValueError("This output has no active mode to restore; choose the active desktop output.")
    if previous == (mode, rate):
        print("That mode is already active.")
        return
    fd, token = tempfile.mkstemp(prefix="noland-display-rollback-")
    os.close(fd)
    # Schedule rollback BEFORE applying a mode: a disconnect or crash must not
    # leave the user stranded. This process survives closure of the terminal.
    try:
        subprocess.Popen(
            [sys.executable, str(Path(__file__).resolve()), "--rollback", token,
             "--output", output, "--mode", previous[0], "--rate", previous[1]],
            start_new_session=True, stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )
    except Exception:
        Path(token).unlink(missing_ok=True)
        raise
    try:
        apply(output, mode, rate)
        if confirm():
            Path(token).unlink(missing_ok=True)
            print("Display mode kept for this session. Reconnect Play if streaming needs to renegotiate.")
        else:
            restore(token, output, *previous)
            print("Previous display mode restored.")
    except BaseException:
        restore(token, output, *previous)
        raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--list", action="store_true", help="List advertised resolutions and refresh rates")
    parser.add_argument("--output")
    parser.add_argument("--mode", help="For example 1920x1080; must already be advertised")
    parser.add_argument("--rate", help="For example 60.00; use the value from --list")
    parser.add_argument("--rollback", help=argparse.SUPPRESS)
    args = parser.parse_args()
    os.environ.setdefault("DISPLAY", ":0")
    if Path("/etc/X11/.Xauthority-noland").exists():
        os.environ.setdefault("XAUTHORITY", "/etc/X11/.Xauthority-noland")
    if args.rollback:
        if not all((args.output, args.mode, args.rate)):
            parser.error("Incomplete rollback request")
        time.sleep(20)
        restore(args.rollback, args.output, args.mode, args.rate)
        return
    if os.geteuid() == 0:
        parser.error("Run as the desktop user, without sudo.")
    outputs = query()
    choices = []
    for output, details in outputs.items():
        for mode, rate in details["modes"]:
            choices.append((output, mode, rate))
            marker = " (current)" if details["current"] == (mode, rate) else ""
            print(f"{len(choices):3}. {output}: {mode} @ {rate} Hz{marker}")
    if not choices:
        raise ValueError("No advertised modes found on the Noland X11 desktop.")
    if args.list:
        return
    if not sys.stdin.isatty():
        parser.error("Changing resolution requires a terminal so you can confirm or revert it.")
    if any((args.output, args.mode, args.rate)):
        if not all((args.output, args.mode, args.rate)):
            parser.error("Provide --output, --mode and --rate together")
        choice = (args.output, args.mode, args.rate)
    else:
        answer = input("Choose a mode number, or press Enter to cancel: ").strip()
        if not answer:
            return
        if not answer.isdigit() or not 1 <= int(answer) <= len(choices):
            raise ValueError("Invalid mode number.")
        choice = choices[int(answer) - 1]
    change(*choice, outputs)


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        print(f"Display change failed: {error}", file=sys.stderr)
        sys.exit(1)
