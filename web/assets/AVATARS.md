# Symbiont avatar display

The supplied raster artwork is preserved unchanged:

- `symbiont-avatar.png`: normal master, 1254 × 1254.
- `input-role-avatars/symbiont-dissent.png`: dissent master, 1254 × 1254.
- `symbiont-avatar-display.png`, `symbiont-avatar-small.png`, and
  `input-role-avatars/symbiont-dissent-small.png`: existing transparent delivery
  images embedded by `src/web.rs`.

Small-size adaptation is non-destructive and belongs to the web presentation:

- `symbiont-avatar-art` limits the image to 90% of its container, making the
  original roughly 89% silhouette occupy about 80% of the circular avatar.
- The shared `symbiont-avatar-midtone` SVG filter in `web/index.html` applies an
  sRGB midtone curve. Black and white endpoints, geometry, and alpha are unchanged.
  There is no alpha transfer and no global exposure/brightness multiplier.
- Normal conversation, Topic chat, the settings preview and dissent use the same
  treatment. Custom/user avatars and other input-role artwork do not. A failed
  custom Symbiont image receives the treatment only when it falls back to the
  bundled normal avatar.

Do not bake the same padding/curve into the PNG derivatives as well, or the
treatment would be applied twice. Since the image bytes have not changed, their
existing immutable cache version remains valid. The HTML, JavaScript and CSS
must be rebuilt into the daemon and reloaded together.

Validation: `node --test web/avatar-assets.test.mjs`, the project's Web gate,
one consumer build, and a browser comparison at 24px, 40px and 76px. Generated
retouch candidates were rejected for geometry/transparency changes and are not
used by the product.
