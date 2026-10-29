# Mise en page du DMG, lue par dmgbuild (appelé par scripts/bundle.sh --dmg).
#
# dmgbuild écrit lui-même le .DS_Store de la fenêtre : aucune automatisation du Finder,
# donc le même résultat en local et sur les machines de CI sans session graphique.
#
# Valeurs passées par bundle.sh avec -D : app (chemin du .app), background (TIFF 1x + 2x),
# icon (icône du volume).

import os.path

app = defines["app"]  # noqa: F821 (fourni par dmgbuild)
app_name = os.path.basename(app)

format = "ULFO"  # compression LZFSE, comme le DMG précédent
files = [app]
symlinks = {"Applications": "/Applications"}
icon = defines["icon"]  # noqa: F821
background = defines["background"]  # noqa: F821

# Fenêtre de 600 × 400 points, à la taille de l'image de fond.
window_rect = ((200, 140), (600, 400))
default_view = "icon-view"
show_status_bar = False
show_tab_view = False
show_toolbar = False
show_pathbar = False
show_sidebar = False
include_icon_view_settings = True

# Centres des icônes : doivent correspondre à la flèche de scripts/make-dmg-background.swift.
icon_size = 128
text_size = 13
icon_locations = {
    app_name: (150, 190),
    "Applications": (450, 190),
}
hide_extension = [app_name]
