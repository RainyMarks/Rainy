"""Write native/crates/app/assets/rainy-theme.json: GPUI Kit's default themes with Rainy's colors
(src/renderer/theme/schemes.css graphite ramp and blue accent; tokens.css surfaces) laid over them.
Usage: python native/scripts/make-theme.py <gpui-component default-theme.json> <out>"""
import json, sys

source, out = sys.argv[1], sys.argv[2]
base = json.load(open(source, encoding="utf-8"))
light = next(t for t in base["themes"] if t["mode"] == "light")
dark = next(t for t in base["themes"] if t["mode"] == "dark")

accent = {"50": "#edf3fe", "100": "#e4edfd", "200": "#d3e2ff", "300": "#b7c8fe", "400": "#7aaaff",
          "450": "#5686fe", "500": "#4176e6", "600": "#4868b2", "800": "#34415b", "900": "#283142"}

dark_colors = {
    "background": "#141414", "foreground": "#ebebeb", "border": "#ffffff17", "caret": "#ebebeb",
    "muted.background": "#262626", "muted.foreground": "#8c8c8c",
    "accent.background": "#ffffff0f", "accent.foreground": "#ebebeb",
    "popover.background": "#212121", "popover.foreground": "#ebebeb",
    "primary.background": accent["500"], "primary.hover.background": accent["450"],
    "primary.active.background": accent["600"], "primary.foreground": "#ffffff",
    "secondary.background": "#262626", "secondary.hover.background": "#2d2d2d",
    "secondary.active.background": "#333333", "secondary.foreground": "#ebebeb",
    "input.border": "#ffffff21", "ring": accent["400"], "selection.background": accent["800"],
    "link.foreground": accent["400"], "link.hover.foreground": accent["300"], "link.active.foreground": accent["400"],
    "list.background": "#1c1c1c", "list.hover.background": "#ffffff0d", "list.active.background": "#ffffff14",
    "list.active.border": "#ffffff00", "list.even.background": "#1c1c1c", "list.head.background": "#1c1c1c",
    "sidebar.background": "#1c1c1c", "sidebar.foreground": "#ebebeb", "sidebar.border": "#ffffff17",
    "sidebar.accent.background": "#ffffff14", "sidebar.accent.foreground": "#ebebeb",
    "sidebar.primary.background": accent["500"], "sidebar.primary.foreground": "#ffffff",
    "title_bar.background": "#1c1c1c", "title_bar.border": "#ffffff17",
    "status_bar.background": "#1c1c1c", "status_bar.border": "#ffffff17",
    "tab.background": "#1c1c1c", "tab.active.background": "#141414", "tab.foreground": "#8c8c8c",
    "tab.active.foreground": "#ebebeb", "tab_bar.background": "#1c1c1c", "tab_bar.segmented.background": "#262626",
    "scrollbar.background": "#00000000", "scrollbar.thumb.background": "#ffffff26", "scrollbar.thumb.hover.background": "#ffffff40",
    "switch.background": "#3a3a3a", "progress_bar.background": accent["400"], "drag_border": accent["400"],
    "drop_target.background": "#7aaaff26", "overlay": "#00000080", "window.border": "#2d2d2d",
    "group_box.background": "#212121", "group_box.foreground": "#ebebeb",
    "danger.background": "#e5484d", "danger.foreground": "#ffffff",
    "success.background": "#30a46c", "success.foreground": "#ffffff",
    "warning.background": "#f5a524", "warning.foreground": "#171717",
    "info.background": accent["500"], "info.foreground": "#ffffff",
    "skeleton.background": "#ffffff14", "table.background": "#1c1c1c", "table.row.border": "#ffffff0f",
}
light_colors = {
    "background": "#ffffff", "foreground": "#171717", "border": "#0000001a", "caret": "#171717",
    "muted.background": "#f0f0f0", "muted.foreground": "#6e6e6e",
    "accent.background": "#0000000a", "accent.foreground": "#171717",
    "popover.background": "#ffffff", "popover.foreground": "#171717",
    "primary.background": accent["500"], "primary.hover.background": accent["450"],
    "primary.active.background": accent["600"], "primary.foreground": "#ffffff",
    "secondary.background": "#f0f0f0", "secondary.hover.background": "#ebebeb",
    "secondary.active.background": "#e3e3e3", "secondary.foreground": "#171717",
    "input.border": "#0000001f", "ring": accent["500"], "selection.background": accent["200"],
    "link.foreground": accent["500"], "link.hover.foreground": accent["600"], "link.active.foreground": accent["500"],
    "list.background": "#ffffff", "list.hover.background": "#0000000a", "list.active.background": "#00000012",
    "list.active.border": "#00000000", "list.even.background": "#fafafa", "list.head.background": "#fafafa",
    "sidebar.background": "#f7f7f7", "sidebar.foreground": "#171717", "sidebar.border": "#0000001a",
    "sidebar.accent.background": "#00000012", "sidebar.accent.foreground": "#171717",
    "sidebar.primary.background": accent["500"], "sidebar.primary.foreground": "#ffffff",
    "title_bar.background": "#f7f7f7", "title_bar.border": "#0000001a",
    "status_bar.background": "#f7f7f7", "status_bar.border": "#0000001a",
    "tab.background": "#f7f7f7", "tab.active.background": "#ffffff", "tab.foreground": "#6e6e6e",
    "tab.active.foreground": "#171717", "tab_bar.background": "#f7f7f7", "tab_bar.segmented.background": "#ebebeb",
    "scrollbar.background": "#00000000", "scrollbar.thumb.background": "#00000026", "scrollbar.thumb.hover.background": "#00000040",
    "switch.background": "#d9d9d9", "progress_bar.background": accent["500"], "drag_border": accent["500"],
    "drop_target.background": "#4176e626", "overlay": "#0000003d", "window.border": "#d9d9d9",
    "group_box.background": "#fafafa", "group_box.foreground": "#171717",
    "danger.background": "#e5484d", "danger.foreground": "#ffffff",
    "success.background": "#30a46c", "success.foreground": "#ffffff",
    "warning.background": "#f5a524", "warning.foreground": "#171717",
    "info.background": accent["500"], "info.foreground": "#ffffff",
    "skeleton.background": "#0000000a", "table.background": "#ffffff", "table.row.border": "#0000000f",
}

def make(theme, name, colors):
    theme = json.loads(json.dumps(theme))
    theme["name"], theme["is_default"] = name, False
    theme["colors"].update(colors)
    # Slightly square corners, as in Rainy 3.x.
    theme["radius"], theme["radius.lg"] = 6, 8
    theme["shadow"] = True
    if theme["mode"] == "dark" and "highlight" in theme:
        theme["highlight"]["editor.background"] = "#141414"
        theme["highlight"]["editor.active_line.background"] = "#1c1c1c"
    return theme

result = {"name": "Rainy", "author": "RainyMarks", "themes": [make(light, "Rainy Light", light_colors), make(dark, "Rainy Dark", dark_colors)]}
json.dump(result, open(out, "w", encoding="utf-8", newline="\n"), ensure_ascii=False, indent=1)
print("wrote", out)
