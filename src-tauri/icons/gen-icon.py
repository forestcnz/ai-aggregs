"""
ai-aggregs 应用图标生成脚本

用法：
  uv run --with pillow python src-tauri/icons/gen-icon.py
  bun run tauri icon src-tauri/icons/icon-1024.png

生成 1024x1024 透明背景源图（四凸饱满云 + 三数据方块），
再由 tauri icon 产出各平台 PNG / ICO / ICNS。
"""
import math
import os
from PIL import Image, ImageDraw

INK = (31, 30, 30, 255)  # #1f1e1e 不透明


def arc_pts(p1, p2, r, n=48):
    """采样一段凸向上的圆弧（圆心在弧下方）"""
    mx, my = (p1[0] + p2[0]) / 2, (p1[1] + p2[1]) / 2
    dx, dy = p2[0] - p1[0], p2[1] - p1[1]
    d = math.hypot(dx, dy)
    h = math.sqrt(max(r * r - (d / 2) ** 2, 0))
    px, py = -dy / d, dx / d
    c1 = (mx + h * px, my + h * py)
    c2 = (mx - h * px, my - h * py)
    cx, cy = c1 if c1[1] > c2[1] else c2
    a1 = math.atan2(p1[1] - cy, p1[0] - cx)
    a2 = math.atan2(p2[1] - cy, p2[0] - cx)
    amid = (a1 + a2) / 2
    if cy + r * math.sin(amid) > cy + 1e-6:
        a2 -= 2 * math.pi if a2 > a1 else -2 * math.pi
    return [(cx + r * math.cos(a1 + (a2 - a1) * (i / n)),
             cy + r * math.sin(a1 + (a2 - a1) * (i / n))) for i in range(n + 1)]


# 云外轮廓：4 段弧 + 底部留口直线
cloud = []
cloud += arc_pts((10, 40), (19, 30), 7)
cloud += arc_pts((19, 30), (32, 26), 9)
cloud += arc_pts((32, 26), (45, 30), 9)
cloud += arc_pts((45, 30), (54, 40), 7)
cloud.append((16, 40))

blocks = [(21, 46, 6, 6), (29, 46, 6, 6), (37, 46, 6, 6)]

SIZE = 1024
img = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))  # 透明背景
draw = ImageDraw.Draw(img)

scale = 18.0
ox, oy = 32, 39


def tx(x):
    return (x - ox) * scale + SIZE / 2


def ty(y):
    return (y - oy) * scale + SIZE / 2


sw = 3.5 * scale
pts = [(tx(x), ty(y)) for (x, y) in cloud]
draw.line(pts, fill=INK, width=int(round(sw)), joint="curve")
r2 = sw / 2
for e in (pts[0], pts[-1]):
    draw.ellipse([e[0] - r2, e[1] - r2, e[0] + r2, e[1] + r2], fill=INK)

for (bx, by, bw, bh) in blocks:
    draw.rectangle([tx(bx), ty(by), tx(bx + bw), ty(by + bh)], fill=INK)

out = os.path.join(os.path.dirname(os.path.abspath(__file__)), "icon-1024.png")
img.save(out)
print("saved", out)
