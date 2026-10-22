"""Draw the installer's welcome/finish page picture: the Rainy icon on the brand's blue, at twice MUI's 164x314.

Usage: python native/installer/make-finish-bitmap.py <icon.png> <finish.bmp>
"""
import sys

from PIL import Image, ImageDraw

WIDTH, HEIGHT = 328, 628
TOP, BOTTOM = (37, 99, 235), (14, 47, 122)

source, target = sys.argv[1], sys.argv[2]
picture = Image.new('RGB', (WIDTH, HEIGHT))
draw = ImageDraw.Draw(picture)
for y in range(HEIGHT):
    t = y / (HEIGHT - 1)
    draw.line([(0, y), (WIDTH, y)], fill=tuple(round(a + (b - a) * t) for a, b in zip(TOP, BOTTOM)))
icon = Image.open(source).convert('RGBA').resize((168, 168), Image.LANCZOS)
picture.paste(icon, ((WIDTH - icon.width) // 2, 150), icon)
picture.save(target, format='BMP')
