#!/usr/bin/env python3
"""Drive the headless QEMU via its monitor socket.
  qemu-ctl.py shot out.png        screenshot
  qemu-ctl.py key <qemu-keyname>  e.g. ret, esc, right, f2, spc, a
  qemu-ctl.py mouse <dx> <dy> [btn]   relative move; btn=1 click
  qemu-ctl.py raw '<monitor command>'
"""
import os, socket, sys, time, subprocess
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SOCK = os.environ.get("BOOTPICK_RUN_DIR", os.path.join(ROOT, ".run")) + "/monitor.sock"

def mon(cmd):
    s = socket.socket(socket.AF_UNIX); s.connect(SOCK); s.settimeout(3)
    time.sleep(0.1)
    try: s.recv(4096)
    except Exception: pass
    s.sendall((cmd + "\n").encode()); time.sleep(0.25)
    out = b""
    try:
        while True:
            d = s.recv(65536)
            if not d: break
            out += d
            if b"(qemu)" in d: break
    except Exception: pass
    s.close()
    return out.decode(errors="replace")

a = sys.argv[1:]
if a[0] == "shot":
    ppm = a[1] + ".ppm"
    mon(f"screendump {ppm}"); time.sleep(0.4)
    subprocess.run([sys.executable, "-I", "-c",
        f"from PIL import Image; Image.open('{ppm}').save('{a[1]}')"], check=True)
    os.remove(ppm); print(a[1])
elif a[0] == "key":
    for k in a[1:]:
        mon(f"sendkey {k}"); time.sleep(0.15)
elif a[0] == "mouse":
    dx, dy = int(a[1]), int(a[2])
    # USB HID reports are clamped to +-127 per event; split big moves.
    while dx or dy:
        sx = max(-100, min(100, dx)); sy = max(-100, min(100, dy))
        mon(f"mouse_move {sx} {sy}"); dx -= sx; dy -= sy
    if len(a) > 3 and a[3] == "1":
        time.sleep(0.15); mon("mouse_button 1"); time.sleep(0.15); mon("mouse_button 0")
elif a[0] == "raw":
    print(mon(a[1]))
