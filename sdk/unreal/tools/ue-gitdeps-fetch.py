#!/usr/bin/env python3
"""Fetch files from Epic's Commit.gitdeps.xml CDN by path prefix.  usage: ue-gitdeps-fetch.py <xml> <outdir> <prefix>..."""
import sys, os, gzip, urllib.request, xml.etree.ElementTree as ET
xml_path, out, prefixes = sys.argv[1], sys.argv[2], sys.argv[3:]
cache = os.path.expanduser('~/repos/ue/gitdeps-cache')
files, blobs, packs, base = [], {}, {}, None
for ev, el in ET.iterparse(xml_path):
    tag = el.tag.split('}')[-1]
    if tag == 'DependencyManifest': base = el.get('BaseUrl')
    elif tag == 'File':
        n = el.get('Name')
        if any(n.startswith(p) for p in prefixes): files.append((n, el.get('Hash')))
    elif tag == 'Blob': blobs[el.get('Hash')] = (el.get('PackHash'), int(el.get('PackOffset')), int(el.get('Size')))
    elif tag == 'Pack': packs[el.get('Hash')] = (el.get('RemotePath'), int(el.get('CompressedSize')), int(el.get('Size')))
    if tag in ('File', 'Blob', 'Pack'): el.clear()
need = {}
total = 0
for n, h in files:
    ph, off, sz = blobs[h]; total += sz
    need.setdefault(ph, []).append((n, off, sz))
dl = sum(packs[p][1] for p in need)
print(f"{len(files)} files, {total/1e6:.1f} MB; {len(need)} packs, {dl/1e6:.1f} MB compressed", flush=True)
for i, (ph, items) in enumerate(need.items()):
    rp, csz, sz = packs[ph]
    cp = os.path.join(cache, ph)
    if not os.path.exists(cp):
        url = f"{base}/{rp}/{ph}"
        with urllib.request.urlopen(url, timeout=120) as r, open(cp + '.tmp', 'wb') as f: f.write(r.read())
        os.rename(cp + '.tmp', cp)
    with gzip.open(cp, 'rb') as g: data = g.read()
    for n, off, s in items:
        p = os.path.join(out, n); os.makedirs(os.path.dirname(p), exist_ok=True)
        with open(p, 'wb') as f: f.write(data[off:off + s])
    if i % 20 == 0: print(f"pack {i+1}/{len(need)}", flush=True)
print("DONE")
