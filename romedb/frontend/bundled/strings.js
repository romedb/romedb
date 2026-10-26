"use strict";
// Fixed bundled code. Arguments are data, never source text.
globalThis.__romedb_bundle = Object.freeze({
  slugify(text) {
    return text.normalize("NFKD").replace(/\p{M}/gu, "").toLowerCase()
      .replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "");
  },
  normalize(text, form) {
    return text.normalize(form);
  }
});
