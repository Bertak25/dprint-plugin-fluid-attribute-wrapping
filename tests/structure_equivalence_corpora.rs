//! Per-dialect structure-equivalence corpora (task 15.3).
//!
//! Requirement 9.5 asks for automated tests that verify markup-structure
//! equivalence between input and output for a representative set of at least
//! five inputs per supported Markup_Dialect, where equivalence means identical
//! element nesting, identical attribute name-value pairs, and identical text
//! content, disregarding only the whitespace the formatter introduces or
//! removes (Requirement 7.5).
//!
//! The corpora below drive the real public seam — the crate-root re-export of
//! `engine::format_document` (gated on the `wasm`
//! feature) — then compare the input against the actual output using the
//! crate's own structural fingerprint machinery (`verify::structural_fingerprint`
//! / `verify::fingerprints_equivalent`), which *is* the Requirement 7.5
//! equivalence relation. Using the plugin's own notion of equivalence keeps the
//! corpora honest: they assert exactly the guarantee the plugin claims, no more
//! and no less.
//!
//! Each dialect's fixtures use a deliberately narrow `line_width` so wrapping is
//! actually triggered — the formatter rewrites the start tags' inter-attribute
//! whitespace — which is the interesting case for a structure-equivalence check.
//! A couple of fixtures per dialect exercise sorting as well, since under
//! sorting the equivalence relation compares attributes as a multiset.
//!
//! This file is given a distinct name to avoid collisions with the sibling
//! corpora tasks (15.1 end-to-end, 15.2 idempotence, 15.4 performance).
//!
//! Requires the `wasm` feature (for `format_document`). Run with:
//! `cargo test --features wasm`.

#![cfg(feature = "wasm")]

use std::path::Path;

use dprint_plugin_fluid_attribute_wrapping::config::{Configuration, SchemeId};
use dprint_plugin_fluid_attribute_wrapping::dialect::Dialect;
use dprint_plugin_fluid_attribute_wrapping::format_document;
use dprint_plugin_fluid_attribute_wrapping::verify::{
    fingerprints_equivalent, structural_fingerprint,
};

/// Build a test configuration. `line_width` is tunable so fixtures can force
/// wrapping; all optional dialects are enabled so every extension resolves.
fn config(line_width: u32, fluid: bool, sorting: bool) -> Configuration {
    Configuration {
        line_width,
        fluid_wrapping: fluid,
        attribute_sorting: sorting,
        attribute_order_scheme: SchemeId::Standard,
        indent_width: 2,
        use_tabs: false,
        vue: true,
        astro: true,
        svelte: true,
    }
}

/// Format `input` for the file at `file_name` and return the effective output
/// bytes, then assert — using the plugin's OWN structural fingerprint relation
/// (Requirement 7.5) — that the output's markup structure is equivalent to the
/// input's. `Ok(None)` means the plugin made no change, in which case the output
/// is the input verbatim and equivalence is trivially satisfied.
///
/// Panics (fails the test) if formatting errors or if the structures diverge.
fn assert_structure_equivalent(file_name: &str, input: &str, config: &Configuration) {
    let result = format_document(Path::new(file_name), input.as_bytes(), None, config)
        .unwrap_or_else(|err| panic!("format_document errored for {file_name:?}: {err}"));

    // Resolve the effective output: `None` == no change == the input verbatim.
    let output_bytes = match result {
        Some(bytes) => bytes,
        None => input.as_bytes().to_vec(),
    };
    let output = String::from_utf8(output_bytes).expect("output is valid UTF-8");

    // Fingerprint input and output with the SAME dialect profile and indent
    // model the engine used, so any difference reflects real structure, not a
    // measurement mismatch. The dialect is resolved from the file extension,
    // exactly as the engine resolves it.
    let ext = Path::new(file_name)
        .extension()
        .and_then(|e| e.to_str())
        .expect("fixture file names carry a supported extension");
    let dialect = Dialect::from_extension(ext, config)
        .unwrap_or_else(|| panic!("extension {ext:?} must resolve to a supported dialect"));
    let profile = dialect.scanner_profile();
    let indent_width = usize::from(config.indent_width);

    let input_fp = structural_fingerprint(input, &profile, indent_width)
        .expect("input fingerprint should scan");
    let output_fp = structural_fingerprint(&output, &profile, indent_width)
        .expect("output fingerprint should scan");

    assert!(
        fingerprints_equivalent(&input_fp, &output_fp, config.attribute_sorting),
        "structure diverged for {file_name:?}\n  input:  {input:?}\n  output: {output:?}",
    );
}

// ---------------------------------------------------------------------------
// HTML — 5 representative inputs.
// ---------------------------------------------------------------------------

#[test]
fn html_structure_equivalence_corpus() {
    // Narrow width to force wrapping of the multi-attribute tags.
    let cfg = config(28, true, false);
    let corpus = [
        // 1. Several quoted attributes on a single element, wrapped across lines.
        r#"<div class="container" id="main" data-role="banner">content</div>"#,
        // 2. Void element with many attributes (self-closing style omitted, HTML).
        r#"<img src="/images/logo.png" alt="Company logo" width="200" height="80">"#,
        // 3. Mixed quoting plus a boolean attribute and an unquoted value.
        r#"<input type="text" name=username required placeholder='Your name'>"#,
        // 4. Nested elements with their own attributes and surrounding text.
        r#"<section id="hero"><a href="/home" rel="noopener" title="Go home">Home</a></section>"#,
        // 5. Comment + raw-text (script) region that must be preserved verbatim,
        //    alongside a wrappable tag.
        "<!-- banner --><nav class=\"top\" id=\"nav\" role=\"navigation\" aria-label=\"Main\">x</nav><script>var a = 1 < 2;</script>",
    ];
    for (i, input) in corpus.iter().enumerate() {
        assert_structure_equivalent(&format!("fixture_{i}.html"), input, &cfg);
    }

    // The same corpus under sorting enabled: equivalence compares attributes as
    // a multiset, so a reorder is still equivalent.
    let sorted = config(28, true, true);
    for (i, input) in corpus.iter().enumerate() {
        assert_structure_equivalent(&format!("sorted_{i}.html"), input, &sorted);
    }
}

// ---------------------------------------------------------------------------
// JSX — 5 representative inputs.
// ---------------------------------------------------------------------------

#[test]
fn jsx_structure_equivalence_corpus() {
    let cfg = config(30, true, false);
    let corpus = [
        // 1. Component with string and braced attributes, self-closing.
        r#"<Button className="primary" onClick={handleClick} disabled={isBusy} />"#,
        // 2. Spread attribute mixed with named attributes (spread captured verbatim).
        r#"<Card {...rest} title="Welcome" footer={<Footer />} elevation={2} />"#,
        // 3. Member-expression component name with attributes.
        r#"<Form.Input name="email" value={email} onChange={setEmail} required />"#,
        // 4. Nested JSX with attributes on both the outer and inner element.
        r#"<ul className="list"><li key="a" data-index={0}>Item</li></ul>"#,
        // 5. Boolean + quoted + braced on a lowercase intrinsic element.
        r#"<input type="checkbox" checked={done} aria-label="Done" readOnly />"#,
    ];
    for (i, input) in corpus.iter().enumerate() {
        assert_structure_equivalent(&format!("fixture_{i}.jsx"), input, &cfg);
    }

    let sorted = config(30, true, true);
    for (i, input) in corpus.iter().enumerate() {
        assert_structure_equivalent(&format!("sorted_{i}.jsx"), input, &sorted);
    }
}

// ---------------------------------------------------------------------------
// TSX — 5 representative inputs (shares the JS-family profile with JSX).
// ---------------------------------------------------------------------------

#[test]
fn tsx_structure_equivalence_corpus() {
    let cfg = config(32, true, false);
    let corpus = [
        // 1. Generic-looking component name with typed-ish braced props.
        r#"<Select value={selected} options={options} onChange={onSelect} multiple />"#,
        // 2. Spread of typed props plus a named string attribute.
        r#"<Panel {...props} heading="Settings" collapsible={true} />"#,
        // 3. Member-expression component with event + string attributes.
        r#"<Layout.Header sticky={true} className="hdr" title="Dashboard" />"#,
        // 4. Nested intrinsic elements carrying attributes.
        r#"<nav className="bar"><a href="/a" aria-current="page">A</a></nav>"#,
        // 5. Mixed boolean, string and braced on an intrinsic element.
        r#"<input name="bio" size={6} placeholder="About you" required />"#,
    ];
    for (i, input) in corpus.iter().enumerate() {
        assert_structure_equivalent(&format!("fixture_{i}.tsx"), input, &cfg);
    }

    let sorted = config(32, true, true);
    for (i, input) in corpus.iter().enumerate() {
        assert_structure_equivalent(&format!("sorted_{i}.tsx"), input, &sorted);
    }
}

// ---------------------------------------------------------------------------
// Vue — 5 representative inputs (enabled via config.vue).
// ---------------------------------------------------------------------------

#[test]
fn vue_structure_equivalence_corpus() {
    let cfg = config(30, true, false);
    let corpus = [
        // 1. Directive bindings (`:prop`) plus a plain class.
        r#"<img :src="url" :alt="caption" class="thumb" loading="lazy">"#,
        // 2. Event handler directive plus a boolean and a static attribute.
        r#"<button @click="submit" :disabled="busy" type="submit" class="btn">Go</button>"#,
        // 3. `v-bind="obj"` spread-like directive captured verbatim, with others.
        r#"<Comp v-bind="attrs" v-if="visible" title="hello" ref="root" />"#,
        // 4. Two-way binding and a braced value on a component.
        r#"<Field v-model="name" :rules="rules" label="Name" placeholder="Enter" />"#,
        // 5. Nested elements with directive + static attributes and text.
        r#"<ul class="menu"><li :key="id" @click="pick(id)">Item</li></ul>"#,
    ];
    for (i, input) in corpus.iter().enumerate() {
        assert_structure_equivalent(&format!("fixture_{i}.vue"), input, &cfg);
    }

    let sorted = config(30, true, true);
    for (i, input) in corpus.iter().enumerate() {
        assert_structure_equivalent(&format!("sorted_{i}.vue"), input, &sorted);
    }
}

// ---------------------------------------------------------------------------
// Astro — 5 representative inputs (enabled via config.astro).
// ---------------------------------------------------------------------------

#[test]
fn astro_structure_equivalence_corpus() {
    let cfg = config(32, true, false);
    let corpus = [
        // 1. Frontmatter region (opaque) followed by a wrappable element.
        "---\nconst title = 'Home';\n---\n<main class=\"page\" id=\"root\" data-env=\"prod\">body</main>",
        // 2. `client:*` directive plus braced and string attributes on a component.
        r#"<Counter client:load initialCount={0} label="Clicks" class="counter" />"#,
        // 3. Spread of props plus named attributes on a component.
        r#"<Card {...props} title="Astro" href="/learn" class="card" />"#,
        // 4. Member-expression component with mixed attribute forms.
        r#"<UI.Button variant="ghost" onClick={noop} disabled>Press</UI.Button>"#,
        // 5. Nested intrinsic elements with attributes and text.
        r#"<section id="list"><a href="/x" rel="prefetch" class="lnk">X</a></section>"#,
    ];
    for (i, input) in corpus.iter().enumerate() {
        assert_structure_equivalent(&format!("fixture_{i}.astro"), input, &cfg);
    }

    let sorted = config(32, true, true);
    for (i, input) in corpus.iter().enumerate() {
        assert_structure_equivalent(&format!("sorted_{i}.astro"), input, &sorted);
    }
}

// ---------------------------------------------------------------------------
// Svelte — 5 representative inputs (enabled via config.svelte).
// ---------------------------------------------------------------------------

#[test]
fn svelte_structure_equivalence_corpus() {
    let cfg = config(30, true, false);
    let corpus = [
        // 1. Event handler directive plus braced and string attributes.
        r#"<button on:click={handle} class="btn" disabled={busy} type="button">Go</button>"#,
        // 2. Spread directive (`{...rest}`) captured verbatim with named attrs.
        r#"<Widget {...rest} title="Hello" value={count} class="w" />"#,
        // 3. Binding directive plus a static attribute.
        r#"<input bind:value={name} placeholder="Name" class="field" required>"#,
        // 4. Class directive plus string attributes on an element.
        r#"<div class:active={isActive} id="box" role="region" aria-label="Box">x</div>"#,
        // 5. Nested elements with directive + static attributes and text.
        r#"<ul class="list"><li on:click={() => pick(i)} data-id={i}>Item</li></ul>"#,
    ];
    for (i, input) in corpus.iter().enumerate() {
        assert_structure_equivalent(&format!("fixture_{i}.svelte"), input, &cfg);
    }

    let sorted = config(30, true, true);
    for (i, input) in corpus.iter().enumerate() {
        assert_structure_equivalent(&format!("sorted_{i}.svelte"), input, &sorted);
    }
}
