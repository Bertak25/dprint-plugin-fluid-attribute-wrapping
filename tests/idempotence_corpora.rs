//! Per-dialect idempotence corpora (task 15.2).
//!
//! For each supported markup dialect — HTML, JSX, TSX, and the enabled optional
//! dialects Vue, Astro, and Svelte — this test formats a fixture containing at
//! least five representative start tags, then formats the result a second time
//! and asserts the second output is byte-for-byte identical to the first
//! (Requirements 8.1, 8.2, 9.4).
//!
//! These drive the public `engine::format_document` seam re-exported at the
//! crate root. Because `format_document` returns
//! `dprint_core::plugins::FormatError`, it (and therefore this whole test) is
//! gated on the `wasm` feature. Run with `cargo test --features wasm`.
//!
//! The optional Vue/Astro/Svelte dialects are enabled through the resolved
//! `Configuration` flags so their extensions resolve (otherwise `.vue`/`.astro`/
//! `.svelte` files would be a byte-for-byte no-op rather than being wrapped).

#![cfg(feature = "wasm")]

use std::path::Path;

use dprint_plugin_fluid_attribute_wrapping::config::{Configuration, SchemeId};
use dprint_plugin_fluid_attribute_wrapping::format_document;

/// Build a test configuration with all optional dialects enabled and a narrow
/// `line_width` so the representative fixtures actually exercise fluid wrapping
/// (the interesting idempotence case) rather than trivially fitting on one line.
///
/// `fluid_wrapping` and `attribute_sorting` are both enabled so the fixtures run
/// through the full sort-then-wrap pipeline, which is where an idempotence
/// regression would most likely surface.
fn config(line_width: u32) -> Configuration {
    Configuration {
        line_width,
        fluid_wrapping: true,
        attribute_sorting: true,
        attribute_order_scheme: SchemeId::Standard,
        indent_width: 2,
        use_tabs: false,
        vue: true,
        astro: true,
        svelte: true,
    }
}

/// Format `input` as the file at `path`, returning the resulting bytes.
///
/// `format_document` signals no change with `Ok(None)`; this helper folds that
/// back into "the bytes are the input unchanged" so callers always receive the
/// effective output regardless of whether a change was made.
fn format(path: &str, input: &[u8], config: &Configuration) -> Vec<u8> {
    match format_document(Path::new(path), input, None, config)
        .expect("format_document should not error on a representative fixture")
    {
        Some(changed) => changed,
        None => input.to_vec(),
    }
}

/// Assert idempotence for one dialect fixture: format the source once, then
/// format that result again, and require the second output to be byte-identical
/// to the first (Requirements 8.1, 8.2).
///
/// `min_tags` guards the corpus requirement itself — each fixture must contain
/// at least five representative start tags (Requirement 9.4). The count is a
/// conservative lower bound (it counts `<name`-style openers, ignoring closing
/// tags), so the assertion only fails if a fixture is genuinely too small.
fn assert_idempotent(label: &str, path: &str, source: &str, config: &Configuration) {
    assert!(
        start_tag_count(source) >= 5,
        "{label}: fixture must contain at least 5 representative start tags, found {}",
        start_tag_count(source)
    );

    let first = format(path, source.as_bytes(), config);
    let second = format(path, &first, config);

    assert_eq!(
        second, first,
        "{label}: second formatting must be byte-identical to the first (idempotence)"
    );

    // The output must remain valid UTF-8 text; a corpus that produced invalid
    // bytes would itself be a defect worth catching here.
    assert!(
        std::str::from_utf8(&first).is_ok(),
        "{label}: formatted output must be valid UTF-8"
    );
}

/// Count opening element start tags in `source` conservatively.
///
/// A start tag opener is a `<` immediately followed by an ASCII letter (closing
/// tags begin `</`, comments `<!`, so those are excluded). This is only used to
/// assert the fixtures meet the "at least five start tags" corpus requirement,
/// not to parse the markup.
fn start_tag_count(source: &str) -> usize {
    let bytes = source.as_bytes();
    let mut count = 0usize;
    let mut i = 0usize;
    while i + 1 < bytes.len() {
        if bytes[i] == b'<' && bytes[i + 1].is_ascii_alphabetic() {
            count += 1;
        }
        i += 1;
    }
    count
}

#[test]
fn html_corpus_is_idempotent() {
    // Representative HTML start tags: quoted values, a boolean attribute, an
    // unquoted value, many attributes forcing a wrap, and a self-closing tag.
    let source = concat!(
        "<section class=\"layout container\" id=\"main\" role=\"region\" aria-label=\"content\">\n",
        "  <a href=\"https://example.com/path\" target=\"_blank\" rel=\"noopener noreferrer\">link</a>\n",
        "  <button type=\"submit\" name=\"save\" value=\"1\" disabled class=\"primary large\">Save</button>\n",
        "  <input type=\"text\" name=\"email\" id=\"email\" placeholder=\"you@example.com\" required />\n",
        "  <img src=\"/images/banner.png\" alt=\"Banner\" width=\"1200\" height=\"400\" loading=\"lazy\" />\n",
        "  <label for=\"email\" class=\"field-label muted\" data-role=\"caption\">Email address</label>\n",
        "</section>\n",
    );
    assert_idempotent("html", "corpus.html", source, &config(60));
}

#[test]
fn jsx_corpus_is_idempotent() {
    // Representative JSX start tags: a component, braced expression values, a
    // spread attribute, a member-expression tag name, and a self-closing tag.
    let source = concat!(
        "<Panel className=\"card shadow\" title=\"Dashboard\" role=\"region\" data-testid=\"panel\">\n",
        "  <Button variant=\"primary\" size=\"large\" onClick={handleClick} disabled={isBusy}>Go</Button>\n",
        "  <Icon.Star className=\"icon highlight\" width={24} height={24} aria-hidden=\"true\" />\n",
        "  <input type=\"text\" name=\"search\" value={query} placeholder=\"Search items\" required />\n",
        "  <Avatar {...avatarProps} className=\"avatar round\" alt=\"User avatar\" size=\"md\" />\n",
        "  <Link href=\"/home\" className=\"nav-link active\" prefetch={true} replace={false}>Home</Link>\n",
    );
    assert_idempotent("jsx", "corpus.jsx", source, &config(60));
}

#[test]
fn tsx_corpus_is_idempotent() {
    // TSX shares the JSX scanner profile; the corpus uses the same attribute
    // forms to exercise the `.tsx` extension path end to end.
    let source = concat!(
        "<Grid className=\"grid cols-3\" gap={16} role=\"grid\" aria-label=\"Items grid\">\n",
        "  <Card title=\"Metrics\" subtitle=\"Last 30 days\" elevated={true} onClick={openCard}>Body</Card>\n",
        "  <Field<string> name=\"username\" label=\"Username\" value={name} onChange={setName} required />\n",
        "  <Chart.Line data={series} width={640} height={320} animate={false} legend=\"bottom\" />\n",
        "  <Toolbar {...toolbarProps} className=\"toolbar sticky\" position=\"top\" dense={true} />\n",
        "  <Badge variant=\"success\" size=\"small\" rounded={true} className=\"badge\" title=\"Ok\">OK</Badge>\n",
    );
    assert_idempotent("tsx", "corpus.tsx", source, &config(60));
}

#[test]
fn vue_corpus_is_idempotent() {
    // Representative Vue template start tags: directive attributes (`v-*`),
    // bound attributes (`:prop`), event handlers (`@event`), and a component.
    let source = concat!(
        "<div class=\"wrapper flex\" id=\"app\" role=\"main\" data-page=\"home\" aria-busy=\"false\">\n",
        "  <MyComponent :title=\"pageTitle\" :items=\"visibleItems\" variant=\"compact\" ref=\"list\" />\n",
        "  <button type=\"button\" class=\"btn primary\" :disabled=\"isLoading\" @click=\"onSubmit\">Send</button>\n",
        "  <input type=\"email\" v-model=\"form.email\" name=\"email\" placeholder=\"Email\" required />\n",
        "  <li v-for=\"item in items\" :key=\"item.id\" class=\"list-item\" :data-id=\"item.id\">row</li>\n",
        "  <img :src=\"logoUrl\" alt=\"Logo\" width=\"120\" height=\"48\" loading=\"lazy\" class=\"logo\" />\n",
        "</div>\n",
    );
    assert_idempotent("vue", "corpus.vue", source, &config(60));
}

#[test]
fn astro_corpus_is_idempotent() {
    // Representative Astro start tags, including `client:*` directives and
    // component identifiers, preceded by frontmatter that must stay opaque.
    let source = concat!(
        "---\n",
        "const title = \"Home\";\n",
        "---\n",
        "<main class=\"layout container\" id=\"root\" role=\"main\" data-theme=\"light\" lang=\"en\">\n",
        "  <Header title={title} subtitle=\"Welcome\" sticky={true} className=\"site-header\" />\n",
        "  <Counter client:load initialCount={0} step={2} className=\"counter widget\" label=\"n\" />\n",
        "  <a href=\"/docs\" class=\"nav-link active\" target=\"_self\" rel=\"next\" data-section=\"docs\">Docs</a>\n",
        "  <img src=\"/hero.avif\" alt=\"Hero image\" width=\"1600\" height=\"600\" loading=\"eager\" />\n",
        "  <Card.Body padding=\"lg\" shadow={true} className=\"card body\" role=\"group\" title=\"B\">x</Card.Body>\n",
        "</main>\n",
    );
    assert_idempotent("astro", "corpus.astro", source, &config(60));
}

#[test]
fn svelte_corpus_is_idempotent() {
    // Representative Svelte start tags: `on:` handlers, `bind:` directives,
    // braced expression values, and a spread attribute.
    let source = concat!(
        "<section class=\"page container\" id=\"home\" role=\"main\" data-route=\"/\" aria-live=\"polite\">\n",
        "  <button type=\"button\" class=\"btn primary\" disabled={loading} on:click={handleClick}>Go</button>\n",
        "  <input type=\"text\" name=\"query\" bind:value={query} placeholder=\"Search\" class=\"field\" />\n",
        "  <Widget {...widgetProps} class=\"widget bordered\" title=\"Stats\" size=\"md\" expanded={open} />\n",
        "  <a href=\"/about\" class=\"nav-link muted\" target=\"_self\" rel=\"author\" data-nav=\"about\">About</a>\n",
        "  <img src=\"/avatar.webp\" alt=\"Avatar\" width=\"64\" height=\"64\" loading=\"lazy\" class=\"avatar\" />\n",
        "</section>\n",
    );
    assert_idempotent("svelte", "corpus.svelte", source, &config(60));
}
