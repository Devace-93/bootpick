#!/usr/bin/env python3
"""Builds assets/i18n/app.json from <lang>.txt files (key<TAB>text, one per line).
Usage: tools/tsv-to-app-json.py DIR   (DIR has keys.txt = English + es.txt, fr.txt, ...)"""
import json, os, sys
d = sys.argv[1]
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
def load(path):
    rows = [l.rstrip("\n").split("\t", 1) for l in open(path, encoding="utf-8") if l.strip()]
    return [k for k, _ in rows], [v for _, v in rows]
keys, en = load(os.path.join(d, "keys.txt"))
out = {"_keys": keys, "en": en}
order = ["es","fr","de","it","pt","nl","sv","pl","cs","ro","el","uk","ru","tr","ar","hi","zh","ja","ko","vi","id","th"]
for code in order:
    p = os.path.join(d, f"{code}.txt")
    if not os.path.exists(p):
        print("missing", code); continue
    k, v = load(p)
    assert k == keys, f"{code}: keys differ"
    out[code] = v
json.dump(out, open(os.path.join(ROOT, "assets/i18n/app.json"), "w", encoding="utf-8"), ensure_ascii=False, indent=1)
print(len(keys), "keys,", len(out) - 1, "languages")
