"""Compare row-major little-endian u16 mosaics in full, active, and crop domains."""
from pathlib import Path
import json
import numpy as np
import sys

if len(sys.argv) != 2:
    raise SystemExit('usage: python3 compare.py PROBE_OUTPUT_DIRECTORY')
base = Path(sys.argv[1])
if (base / 'comparison.json').exists():
    raise SystemExit('comparison.json already exists; use a new probe directory')
results = []
for rp in sorted(base.glob('*-rawler/result.json')):
    name = rp.parent.name.removesuffix('-rawler')
    a = json.loads(rp.read_text())
    bp = base / (name + '-libraw')
    b = json.loads((bp / 'result.json').read_text())
    w, h = a['width'], a['height']
    if (w, h) != (b['raw_width'], b['raw_height']):
        results.append({'name': name, 'error': 'dimensions differ'})
        continue
    def box(rect):
        return rect['p']['x'],rect['p']['y'],rect['p']['x']+rect['d']['w'],rect['p']['y']+rect['d']['h']
    ax0,ay0,ax1,ay1=box(a['active_area'])
    x0,y0,x1,y1=box(a['crop_area'])
    regions = {
        'whole': (0,0,w,h),
        'rawler_active':(ax0,ay0,ax1,ay1),
        'rawler_crop':(x0,y0,x1,y1),
        'libraw_active':(b['left_margin'],b['top_margin'],b['left_margin']+b['width'],b['top_margin']+b['height']),
    }
    ar=np.memmap(rp.parent/'samples.u16le',dtype='<u2',mode='r',shape=(h,w))
    br=np.memmap(bp/'samples.u16le',dtype='<u2',mode='r',shape=(h,w))
    buckets={}
    for key,(rx0,ry0,rx1,ry1) in regions.items():
        s={'count':0,'mismatch':0,'max_abs':0,'abs_sum':0,'first':[]}
        for cy in range(ry0,ry1,256):
            ey=min(cy+256,ry1)
            va=ar[cy:ey,rx0:rx1].astype(np.int32)
            vb=br[cy:ey,rx0:rx1].astype(np.int32)
            d=np.abs(va-vb)
            s['count']+=d.size;s['mismatch']+=int(np.count_nonzero(d));s['max_abs']=max(s['max_abs'],int(d.max()));s['abs_sum']+=int(d.sum())
            if len(s['first'])<5:
                ys,xs=np.nonzero(d)
                for iy,ix in list(zip(ys,xs))[:5-len(s['first'])]:
                    s['first'].append({'x':int(rx0+ix),'y':int(cy+iy),'rawler':int(va[iy,ix]),'libraw':int(vb[iy,ix])})
        s['mean_abs']=s['abs_sum']/s['count'] if s['count'] else None
        buckets[key]=s
    results.append({'name':name,'dimensions':[w,h],'regions':buckets})
    print(name, [(key,buckets[key]['mismatch'],buckets[key]['max_abs']) for key in buckets],flush=True)
(base / 'comparison.json').write_text(json.dumps(results,indent=2)+'\n')
