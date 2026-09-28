# dmgbuild settings for the drag-to-install window (see build-dmg.sh).
# dmgbuild writes the window layout straight into the image's .DS_Store,
# so no Finder scripting is involved. Pass -D app=<path to .app>.
import os.path

app = defines["app"]  # noqa: F821 - injected by dmgbuild
app_name = os.path.basename(app)

format = "UDZO"
compression_level = 9
files = [app]
symlinks = {"Applications": "/Applications"}

# Matches icons/dmg-background.tiff (540x380 points, with an @2x layer).
background = "icons/dmg-background.tiff"  # relative to app/, where build-dmg.sh runs
window_rect = ((200, 120), (540, 380))
show_status_bar = False
show_tab_view = False
show_toolbar = False
show_pathbar = False
show_sidebar = False
default_view = "icon-view"
arrange_by = None
icon_size = 112
text_size = 13
show_item_info = False
icon_locations = {app_name: (140, 170), "Applications": (400, 170)}
