"""Independent per-pixel SSIM/PSNR for the score.rs golden test (direct from the definition)."""
import math
import numpy as np

W, H = 24, 20
a = np.zeros((H, W, 3))
b = np.zeros((H, W, 3))
for y in range(H):
    for x in range(W):
        for c in range(3):
            v = (x * 37 + y * 91 + x * y * 13 + c * 50) % 256
            d = ((x * 5 + y * 3 + c) % 17) - 8
            a[y, x, c] = v
            b[y, x, c] = min(255, max(0, v + d))

sigma = 1.5
g = np.array([math.exp(-((i - 5) ** 2) / (2 * sigma * sigma)) for i in range(11)])
g /= g.sum()
win = np.outer(g, g)
C1 = (0.01 * 255) ** 2
C2 = (0.03 * 255) ** 2


def ssim_at(x, y, c):
    ys = np.clip(np.arange(y - 5, y + 6), 0, H - 1)
    xs = np.clip(np.arange(x - 5, x + 6), 0, W - 1)
    pa = a[np.ix_(ys, xs, [c])][:, :, 0]
    pb = b[np.ix_(ys, xs, [c])][:, :, 0]
    ma = (win * pa).sum()
    mb = (win * pb).sum()
    va = (win * pa * pa).sum() - ma * ma
    vb = (win * pb * pb).sum() - mb * mb
    cov = (win * pa * pb).sum() - ma * mb
    return ((2 * ma * mb + C1) * (2 * cov + C2)) / ((ma * ma + mb * mb + C1) * (va + vb + C2))


region = np.zeros((H, W), bool)
region[5:12, 6:14] = True
ringw = 2
ring = np.zeros_like(region)
for y in range(H):
    for x in range(W):
        if not region[y, x]:
            continue
        for dy in range(-ringw, ringw + 1):
            for dx in range(-ringw, ringw + 1):
                yy, xx = y + dy, x + dx
                if 0 <= yy < H and 0 <= xx < W and not region[yy, xx]:
                    ring[y, x] = True


def report(mask):
    ys, xs = np.nonzero(mask)
    s = np.mean([np.mean([ssim_at(x, y, c) for c in range(3)]) for y, x in zip(ys, xs)])
    mse = np.mean([(a[y, x, c] - b[y, x, c]) ** 2 for y, x in zip(ys, xs) for c in range(3)])
    return len(ys), 10 * math.log10(255 * 255 / mse), s


for name, m in (("region", region), ("ring", ring)):
    n, p, s = report(m)
    print(name, n, repr(p), repr(s))
