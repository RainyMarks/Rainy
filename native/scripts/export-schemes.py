"""Write native/crates/app/assets/schemes.json: the gray ramp of every color scheme and the ramp of every accent from
src/renderer/theme/schemes.css, as hex colors. Run from the repository root: python native/scripts/export-schemes.py"""
import json
import re
from pathlib import Path

css = Path("src/renderer/theme/schemes.css").read_text(encoding="utf-8")
blocks = re.findall(r"([^{}]+)\{([^{}]*)\}", css)
schemes, accents = {}, {}


def hex_color(value):
    match = re.fullmatch(r"rgb\((\d+),\s*(\d+),\s*(\d+)\)", value.strip())
    if not match:
        raise ValueError(value)
    return "#" + "".join(f"{int(part):02x}" for part in match.groups())


for selector, body in blocks:
    names = re.findall(r"\[data-(color-scheme|accent)='([a-z]+)'\]", selector)
    values = dict(re.findall(r"--rainy-static-((?:gray|accent)-[0-9a-z-]+):\s*([^;]+);", body))
    if not values:
        continue
    if selector.strip().startswith("body"):
        # `body, [data-…='graphite']` / `body, [data-accent='blue']`: the defaults are named by their attribute.
        pass
    for kind, name in names:
        target = schemes if kind == "color-scheme" else accents
        ramp = {key.split("-", 1)[1].replace("-delete", ""): hex_color(value) for key, value in values.items()}
        target.setdefault(name, {}).update(ramp)

out = Path("native/crates/app/assets/schemes.json")
out.write_text(json.dumps({"schemes": schemes, "accents": accents}, indent=1) + "\n", encoding="utf-8", newline="\n")
print("schemes:", ", ".join(schemes), "| accents:", ", ".join(accents))
