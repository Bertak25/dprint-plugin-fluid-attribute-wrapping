# Requirements Document

## Introduction

This feature is a [dprint](https://dprint.dev/) Wasm formatting plugin that performs a post-processing pass over markup produced by major web framework formatters (HTML, JSX, and related markup such as TSX, Vue, Astro, and Svelte templates). The plugin exists to change one specific formatting behavior: how element attributes wrap when a start tag exceeds the configured line width.

Mainstream formatters (Prettier and most dprint markup plugins) use an "all-or-nothing" strategy — when attributes do not fit on one line, every attribute is placed on its own line. This plugin instead applies **fluid (greedy) wrapping**: it packs as many attributes as fit on each line up to the line width, then continues the remaining attributes on following lines. The plugin also offers an optional, toggleable feature to reorder element attributes according to a conventional ordering scheme.

The plugin uses a purpose-built start-tag scanner rather than a full JavaScript lexer, so JSX and TSX attribute values that are genuinely ambiguous braced expressions (for example those whose nested braces, template literals, regex literals, or comments leave the scanner's lexical state unresolvable) are handled conservatively: the plugin leaves such start tags unchanged rather than claiming full JSX attribute coverage. Where an attribute value can be delimited with full confidence — including an attribute value that holds a genuine JSX element, whose inner element may carry a closing `</…>` end tag — the plugin recognizes the nested element and formats BOTH the enclosing start tag and every nested element, to arbitrary depth, rather than treating the enclosing tag as a no-op. A structure-equivalence self-check protects every rewrite at both levels, causing the plugin to fall back to leaving the input unchanged whenever it cannot confidently rewrite a tag.

The plugin is distributed as a sandboxed WebAssembly plugin implementing the dprint Wasm plugin interface (Schema Version 4) using the `dprint-core` crate, is strictly typed, follows current (2026) Rust and dprint best practices, and ships with an automated test suite.

The plugin guarantees idempotence only for its own output (an intra-plugin fixed point); cross-plugin convergence across the dprint formatting loop is out of scope and depends on Upstream_Plugins not re-expanding Start_Tags the plugin has already wrapped, so users should not expect the plugin to control CLI-loop convergence or file "infinite reformat" bugs that arise from such cross-plugin oscillation.

## Glossary

- **Plugin**: The dprint Wasm plugin defined by this specification, named the Fluid_Attribute_Wrapping_Plugin.
- **Fluid_Attribute_Wrapping_Plugin**: The system under specification; the Wasm module that reflows and optionally reorders markup element attributes. Also referred to as "the Plugin".
- **dprint_CLI**: The dprint host process that loads plugins, resolves configuration, and dispatches format requests to plugins.
- **Format_Request**: The input passed by the dprint_CLI to the Plugin, containing the file path, file bytes, an optional selection range, and resolved configuration.
- **Upstream_Plugin**: Any other dprint plugin (for example `npm:@dprint/typescript` or `dprint-plugin-markup`) that formats a file before the Plugin runs on the same file.
- **Post_Processing_Pass**: A formatting run in which the Plugin receives markup that an Upstream_Plugin has already formatted, and applies additional transformations rather than being the sole formatter for the file.
- **Additive_Mode**: The dprint plugin behavior in which a plugin formats a matched file in addition to the plugin that claims the file, rather than claiming the file exclusively. Controlled by the `additive` flag in the plugin's file-matching information.
- **Line_Width**: The maximum number of columns a line of formatted output should occupy, expressed as a positive integer count of characters.
- **Start_Tag**: The opening tag of a markup element, including the tag name and its attributes (for example `<div class="a" id="b">`).
- **Attribute**: A single name, name-value, or dynamic/spread entry within a Start_Tag (for example `class="a"`, `disabled`, `{...props}`, `v-if="x"`).
- **Fluid_Wrapping**: An attribute layout strategy that places as many Attributes as fit within Line_Width on each line, then continues remaining Attributes on subsequent lines.
- **All_Or_Nothing_Wrapping**: An attribute layout strategy that places every Attribute of an overflowing Start_Tag on its own line.
- **Attribute_Sorting**: The optional, toggleable transformation that reorders the Attributes of a Start_Tag according to a configured ordering scheme.
- **Attribute_Order_Scheme**: A named, ordered set of Attribute grouping rules that defines the relative position of Attributes when Attribute_Sorting is enabled.
- **standard**: The single built-in Attribute_Order_Scheme shipped by the Plugin, and the default scheme used when Attribute_Sorting is enabled and no scheme is explicitly configured.
- **Complex_Expression_Attribute**: A JSX or TSX Attribute whose value is a braced expression that the Plugin's Start_Tag scanner cannot tokenize with full confidence, for example an expression containing nested braces, a template literal, a regex literal, or a comment.
- **Markup_Dialect**: A specific markup language the Plugin supports, one of HTML, JSX, TSX, Vue template, Astro, or Svelte.
- **Format_Error**: A structured error the Plugin returns to the dprint_CLI when it cannot complete a Format_Request.
- **Configuration_Diagnostic**: A message the Plugin returns describing an invalid or unknown configuration property.

## Requirements

### Requirement 1: dprint Wasm Plugin Interface Conformance

**User Story:** As a developer integrating the Plugin into a dprint setup, I want the Plugin to conform to the dprint Wasm plugin interface, so that the dprint_CLI can load, configure, and invoke it like any other plugin.

#### Acceptance Criteria

1. THE Fluid_Attribute_Wrapping_Plugin SHALL implement the dprint Wasm plugin interface at Schema Version 4.
2. WHEN the dprint_CLI requests plugin information, THE Fluid_Attribute_Wrapping_Plugin SHALL return a non-empty plugin name, a plugin version matching the pattern MAJOR.MINOR.PATCH (each a non-negative integer), a non-empty configuration key, a help URL, and a configuration schema URL, where each URL is an absolute URL beginning with `http://` or `https://`.
3. WHEN the dprint_CLI requests the plugin license text, THE Fluid_Attribute_Wrapping_Plugin SHALL return non-empty license text.
4. WHEN the dprint_CLI requests file-matching information, THE Fluid_Attribute_Wrapping_Plugin SHALL return the set of supported file extensions derived from the resolved configuration.
5. IF the dprint_CLI requests file-matching information before a configuration has been resolved, THEN THE Fluid_Attribute_Wrapping_Plugin SHALL return an error response indicating that no resolved configuration is available, and SHALL leave any previously resolved configuration unchanged.
6. THE Fluid_Attribute_Wrapping_Plugin SHALL compile to a single `wasm32-unknown-unknown` WebAssembly module that exports every function the dprint Wasm plugin interface at Schema Version 4 requires.

### Requirement 2: Post-Processing Execution After Upstream Plugins

**User Story:** As a developer, I want the Plugin to run after other dprint plugins have formatted a file, so that it refines already-formatted markup instead of competing with the primary formatter.

#### Acceptance Criteria

1. THE Fluid_Attribute_Wrapping_Plugin SHALL report file-matching information that enables Additive_Mode, so that the dprint_CLI runs the Plugin in addition to the Upstream_Plugin that claims a matched file.
2. WHEN the dprint_CLI dispatches a Format_Request for a file already formatted by an Upstream_Plugin, THE Fluid_Attribute_Wrapping_Plugin SHALL accept the file bytes produced by the Upstream_Plugin as input and apply its transformations to that input.
3. WHERE no Upstream_Plugin has formatted a supported file, THE Fluid_Attribute_Wrapping_Plugin SHALL accept the original file bytes as input and apply the same transformations that it applies in Additive_Mode.
4. WHEN the Fluid_Attribute_Wrapping_Plugin produces output byte-for-byte identical to the input it received, THE Fluid_Attribute_Wrapping_Plugin SHALL signal to the dprint_CLI that no change was made.
5. IF the input received by the Fluid_Attribute_Wrapping_Plugin is not valid UTF-8 text, THEN THE Fluid_Attribute_Wrapping_Plugin SHALL return a Format_Error indicating the input could not be decoded and SHALL leave the input unchanged.
6. THE Fluid_Attribute_Wrapping_Plugin documentation SHALL state that the Plugin should be listed after all Upstream_Plugins in the dprint configuration `plugins` array for the intended execution order, noting that this ordering is a soft recommendation that affects only the quality of the combined formatting result and not the Plugin's own correctness or safety.
7. IF the Fluid_Attribute_Wrapping_Plugin receives input that an Upstream_Plugin has not yet formatted, for example because of plugin-order misconfiguration, THEN THE Fluid_Attribute_Wrapping_Plugin SHALL apply its transformations correctly to that input and SHALL derive its correctness and safety guarantees from the received input alone, independent of Upstream_Plugin ordering.

### Requirement 3: Supported Markup Dialects

**User Story:** As a developer working across web frameworks, I want the Plugin to support the major markup formats, so that attribute wrapping is consistent across my codebase.

#### Acceptance Criteria

1. THE Fluid_Attribute_Wrapping_Plugin SHALL support the HTML Markup_Dialect for files whose extension, matched case-insensitively, is `html`.
2. THE Fluid_Attribute_Wrapping_Plugin SHALL support the JSX and TSX Markup_Dialects for files whose extension, matched case-insensitively, is `jsx` or `tsx` respectively.
3. WHERE the Markup_Dialect for Vue, Astro, or Svelte is enabled in the resolved configuration, THE Fluid_Attribute_Wrapping_Plugin SHALL process Start_Tags within files whose extension, matched case-insensitively, is `vue`, `astro`, or `svelte` respectively.
4. WHERE the Markup_Dialect for Vue, Astro, or Svelte is not enabled in the resolved configuration, THE Fluid_Attribute_Wrapping_Plugin SHALL preserve the input content of files with the `vue`, `astro`, or `svelte` extension byte-for-byte and signal that no change was made.
5. WHEN a Format_Request targets a file whose extension is not in the resolved supported extension set, THE Fluid_Attribute_Wrapping_Plugin SHALL preserve the input content byte-for-byte and signal that no change was made.
6. WHEN the Fluid_Attribute_Wrapping_Plugin processes a supported file, THE Fluid_Attribute_Wrapping_Plugin SHALL preserve all markup content outside of Start_Tags byte-for-byte.
7. WHEN the Fluid_Attribute_Wrapping_Plugin processes a JSX or TSX file and encounters a `<` whose immediately preceding significant token is a value producer (an identifier character, `)`, `]`, or a string, numeric, or template literal), such as the `<` in a TypeScript generic argument list (`Array<Task>`, `useState<...>`, `axios.get<...>`) or a `<` comparison operator (`a < b`), THE Fluid_Attribute_Wrapping_Plugin SHALL NOT treat that `<` as the start of a Start_Tag and SHALL preserve the construct, including every `<` and `>` character, byte-for-byte.
8. WHEN the Fluid_Attribute_Wrapping_Plugin processes a JSX or TSX file and encounters a TypeScript generic argument list, generic type argument, or `<` comparison operator inside either a child-position `{ ... }` expression container or an attribute-value `attr={ ... }` expression container, THE Fluid_Attribute_Wrapping_Plugin SHALL recognize genuine JSX elements within that container while leaving any such non-element `<` construct byte-for-byte unchanged.
9. WHEN the Fluid_Attribute_Wrapping_Plugin processes a JSX or TSX file and encounters a component Start_Tag whose element name is immediately followed by a generic `<` (for example `<EditableList<SearchEngineDomain>>` or `<Select<Array<number>> ...>`) that the Start_Tag scanner cannot delimit with full confidence, THE Fluid_Attribute_Wrapping_Plugin SHALL mark that Start_Tag not-confidently-rewritable and leave it byte-for-byte unchanged as a graceful per-tag no-op rather than dropping any `<` or `>`, both when such a component-with-generics head is the enclosing Start_Tag and when it is a nested element inside an attribute-value container.
10. WHEN the Fluid_Attribute_Wrapping_Plugin processes a JSX or TSX file and encounters an enclosing Start_Tag one of whose attribute values is a braced expression that contains a genuine JSX element (for example `primary={ <a href=…>…</a> }` or `secondaryAction={ <Stack …><IconButton … /></Stack> }`), THE Fluid_Attribute_Wrapping_Plugin SHALL recognize that nested element — at any depth, including element-bearing attribute values nested inside element-bearing attribute values — and SHALL process BOTH levels: it SHALL format the enclosing Start_Tag's OWN top-level attributes identically to the same Start_Tag with no element-bearing attribute value, AND it SHALL recognize and format every nested element identically to the same element placed outside any container, while preserving all non-element attribute content (template literals, call expressions, arrow functions, ternaries), any TypeScript generic or `<` comparison operator appearing inside an attribute value, dynamic or spread attributes, and component-with-generics heads byte-for-byte.
11. WHEN the Fluid_Attribute_Wrapping_Plugin delimits an attribute-value braced expression whose inner JSX element carries a closing `</…>` end tag, THE Fluid_Attribute_Wrapping_Plugin SHALL delimit that attribute value with full confidence — recognizing a `/` that immediately follows a `<` as a JSX closing-tag slash rather than the start of a regular-expression literal — so the enclosing Start_Tag remains confidently rewritable and is NOT left as a per-tag no-op merely because one of its attribute values contains an element with an end tag.

### Requirement 4: Fluid (Greedy) Attribute Wrapping

**User Story:** As a developer, I want attributes to wrap fluidly rather than one-per-line, so that start tags stay compact and readable.

#### Acceptance Criteria

1. WHEN a Start_Tag together with its Attributes and its closing bracket occupies a column count less than or equal to Line_Width on a single line, THE Fluid_Attribute_Wrapping_Plugin SHALL keep that Start_Tag on a single line.
2. WHEN a Start_Tag together with its Attributes and its closing bracket occupies a column count greater than Line_Width, THE Fluid_Attribute_Wrapping_Plugin SHALL apply Fluid_Wrapping by placing on each line the maximum number of consecutive remaining Attributes whose combined column count keeps that line less than or equal to Line_Width, then continuing the remaining Attributes on the next line.
3. WHILE applying Fluid_Wrapping to a Start_Tag, THE Fluid_Attribute_Wrapping_Plugin SHALL place at least one Attribute on every continuation line that contains Attributes, even when that single Attribute causes the line to exceed Line_Width.
4. WHEN the Fluid_Attribute_Wrapping_Plugin wraps a Start_Tag, THE Fluid_Attribute_Wrapping_Plugin SHALL indent each continuation line of that Start_Tag by exactly one additional indentation unit, as defined by the resolved indentation configuration, relative to the indentation column of the line on which the Start_Tag begins.
5. IF a single Attribute by itself occupies a column count greater than Line_Width, THEN THE Fluid_Attribute_Wrapping_Plugin SHALL place that Attribute on its own line without truncating or splitting the Attribute content.
6. WHEN the Fluid_Attribute_Wrapping_Plugin wraps a Start_Tag, THE Fluid_Attribute_Wrapping_Plugin SHALL preserve the parsed value and identity of every Attribute present before wrapping.
7. WHERE a Start_Tag contains zero Attributes, THE Fluid_Attribute_Wrapping_Plugin SHALL keep that Start_Tag on a single line without applying Fluid_Wrapping.
8. WHERE the resolved configuration disables Fluid_Wrapping, THE Fluid_Attribute_Wrapping_Plugin SHALL leave attribute line breaks produced by the Upstream_Plugin unchanged.
9. WHERE a Start_Tag contains a Complex_Expression_Attribute whose braced expression the Start_Tag scanner cannot delimit with full confidence, THE Fluid_Attribute_Wrapping_Plugin SHALL leave that Start_Tag unchanged byte-for-byte as a graceful no-op rather than rewriting it.
10. WHEN the Fluid_Attribute_Wrapping_Plugin processes a JSX or TSX file and encounters a genuine JSX element Start_Tag located inside a child-position `{ ... }` expression container (for example within a `.map()` or other callback, a ternary, an `&&`/`||` conditional, or a nested container), THE Fluid_Attribute_Wrapping_Plugin SHALL recognize that Start_Tag and apply Fluid_Wrapping, and where enabled Attribute_Sorting, to it identically to the same Start_Tag placed outside any container, while preserving the surrounding container expression text byte-for-byte.
11. WHEN the Fluid_Attribute_Wrapping_Plugin processes a JSX or TSX file and encounters an enclosing Start_Tag one of whose attribute values is a braced expression `attr={ ... }` that contains a genuine JSX element, THE Fluid_Attribute_Wrapping_Plugin SHALL apply Fluid_Wrapping, and where enabled Attribute_Sorting, at BOTH levels: (a) to the enclosing Start_Tag's OWN top-level attributes, identically to the same enclosing Start_Tag whose attribute value held no element, AND (b) to every nested element discovered inside the attribute value, at any depth, identically to the same element placed outside any container — while preserving the non-element portions of the attribute value and the surrounding start-tag text byte-for-byte. THE Fluid_Attribute_Wrapping_Plugin SHALL NOT leave the enclosing Start_Tag unformatted merely because one of its attribute values contains a nested element.

### Requirement 5: Toggleable Attribute Sorting

**User Story:** As a developer, I want to optionally sort element attributes into a conventional order, so that markup is consistent and easier to scan.

#### Acceptance Criteria

1. WHERE Attribute_Sorting is enabled in the resolved configuration, THE Fluid_Attribute_Wrapping_Plugin SHALL reorder the Attributes of each Start_Tag according to the configured Attribute_Order_Scheme.
2. WHERE Attribute_Sorting is disabled in the resolved configuration, THE Fluid_Attribute_Wrapping_Plugin SHALL preserve the original source order of Attributes within each Start_Tag.
3. WHEN the Fluid_Attribute_Wrapping_Plugin sorts the Attributes of a Start_Tag, THE Fluid_Attribute_Wrapping_Plugin SHALL retain every Attribute and its value, adding and removing no Attributes.
4. WHEN two Attributes belong to the same group of the Attribute_Order_Scheme, THE Fluid_Attribute_Wrapping_Plugin SHALL preserve their original source order relative to each other unless the scheme defines an explicit intra-group ordering rule, in which case it SHALL apply that rule.
5. WHERE reordering an Attribute would change rendered or runtime behavior, specifically where two or more Attributes share the same name (duplicate attributes) or where a spread or dynamic Attribute overrides the value of an earlier Attribute, THE Fluid_Attribute_Wrapping_Plugin SHALL preserve the original relative source order of those mutually behavior-affecting Attributes.
6. WHEN Attribute_Sorting and Fluid_Wrapping are both enabled, THE Fluid_Attribute_Wrapping_Plugin SHALL apply Attribute_Sorting before computing Fluid_Wrapping line breaks.
7. IF the configured Attribute_Order_Scheme is unrecognized or structurally invalid, THEN THE Fluid_Attribute_Wrapping_Plugin SHALL preserve the original source order of all Attributes and emit a Configuration_Diagnostic indicating the scheme could not be applied.
8. WHEN an Attribute does not match any group defined in the Attribute_Order_Scheme, THE Fluid_Attribute_Wrapping_Plugin SHALL place that Attribute after all Attributes that match a defined group, preserving the original source order among such unmatched Attributes.
9. WHEN the Fluid_Attribute_Wrapping_Plugin sorts the same Start_Tag more than once with an identical resolved configuration, THE Fluid_Attribute_Wrapping_Plugin SHALL produce an identical ordering of Attributes on each run.

### Requirement 6: Configuration

**User Story:** As a developer, I want to configure the Plugin through the dprint configuration file, so that I can control wrapping and sorting behavior per project.

#### Acceptance Criteria

1. WHEN the dprint_CLI resolves the Plugin configuration, THE Fluid_Attribute_Wrapping_Plugin SHALL read the Line_Width from the plugin configuration as an integer between 1 and 1000 inclusive, defaulting to the dprint global Line_Width when the plugin-level value is absent.
2. THE Fluid_Attribute_Wrapping_Plugin SHALL expose a boolean configuration property that enables or disables Fluid_Wrapping, defaulting to enabled.
3. THE Fluid_Attribute_Wrapping_Plugin SHALL expose a boolean configuration property that enables or disables Attribute_Sorting, defaulting to disabled.
4. THE Fluid_Attribute_Wrapping_Plugin SHALL expose a configuration property that selects the Attribute_Order_Scheme used when Attribute_Sorting is enabled, accepting only a defined Attribute_Order_Scheme value and defaulting to the `standard` scheme.
5. WHEN the resolved configuration contains a property the Fluid_Attribute_Wrapping_Plugin does not recognize, THE Fluid_Attribute_Wrapping_Plugin SHALL return a Configuration_Diagnostic identifying that property, one diagnostic per unrecognized property.
6. WHEN the resolved configuration contains a value of an invalid type or outside the allowed range for a known property, THE Fluid_Attribute_Wrapping_Plugin SHALL return a Configuration_Diagnostic identifying that property together with its expected type and allowed range, one diagnostic per offending property.
7. WHEN the Fluid_Attribute_Wrapping_Plugin returns one or more Configuration_Diagnostics, THE Fluid_Attribute_Wrapping_Plugin SHALL apply the default value for each affected property while retaining the values of all valid properties.
8. THE Fluid_Attribute_Wrapping_Plugin SHALL publish a JSON configuration schema that describes every configuration property, its type, its allowed values or range, and its default value.

### Requirement 7: Error Handling and Robustness

**User Story:** As a developer, I want the Plugin to fail safely on malformed input, so that a formatting pass never corrupts my files.

#### Acceptance Criteria

1. IF the Fluid_Attribute_Wrapping_Plugin encounters a hard parse failure in the input markup, specifically an unterminated Start_Tag (a `<name` with no closing `>` before end of input), an unterminated quoted Attribute value, an unterminated braced expression region, or an unterminated opaque region such as a comment, CDATA section, or raw-text element at end of input, THEN THE Fluid_Attribute_Wrapping_Plugin SHALL return a Format_Error that identifies the line and column of the parse failure and SHALL return the input bytes unchanged.
2. IF a Format_Request specifies a selection range that the Fluid_Attribute_Wrapping_Plugin does not support, THEN THE Fluid_Attribute_Wrapping_Plugin SHALL signal that no change was made.
3. IF a Format_Request specifies a selection range with offsets outside the bounds of the input, THEN THE Fluid_Attribute_Wrapping_Plugin SHALL return a Format_Error identifying the out-of-bounds range and SHALL leave the input unchanged.
4. WHEN the Fluid_Attribute_Wrapping_Plugin encounters a Start_Tag containing dynamic or spread Attributes, THE Fluid_Attribute_Wrapping_Plugin SHALL preserve the content of those Attributes byte-for-byte.
5. WHEN the Fluid_Attribute_Wrapping_Plugin completes a Format_Request, THE Fluid_Attribute_Wrapping_Plugin SHALL return output whose markup structure (element sequence, nesting order, and attribute name-value pairs) is identical to that of the input markup.
6. WHEN the per-tag structure-equivalence self-check, which fingerprints each rewritten Start_Tag individually, detects that a specific rewritten Start_Tag diverged structurally from its original, THE Fluid_Attribute_Wrapping_Plugin SHALL discard the rewrite for that Start_Tag only by restoring its original bytes and signaling no change for that Start_Tag, SHALL retain valid rewrites for all other Start_Tags, and SHALL treat that per-tag divergence as a safe no-op for that Start_Tag rather than a Format_Error.
7. IF a whole-document structure divergence in element sequence, nesting order, or attribute name-value pairs still remains after reverting every individual Start_Tag identified by the per-tag self-check described in acceptance criterion 6, THEN THE Fluid_Attribute_Wrapping_Plugin SHALL return a Format_Error and SHALL leave the input unchanged.
8. WHEN the Fluid_Attribute_Wrapping_Plugin performs its structure-equivalence self-check, THE Fluid_Attribute_Wrapping_Plugin SHALL enforce an angle-bracket-conservation invariant over the whole buffer, requiring the total count of `<` characters and the total count of `>` characters in the output to equal those in the input, and SHALL treat any divergence as a structure divergence that triggers the per-tag revert of acceptance criterion 6 where it is localizable to a single rewritten Start_Tag, or the Format_Error of acceptance criterion 7 otherwise, in no case emitting the changed bytes.
9. WHEN the Fluid_Attribute_Wrapping_Plugin renders any Start_Tag over valid UTF-8 input, including input that places a multibyte (non-ASCII) character immediately before an Attribute occurrence in the sorting-enabled and wrapping-disabled path, THE Fluid_Attribute_Wrapping_Plugin SHALL perform every string slice on a UTF-8 character boundary and SHALL complete the Format_Request as a well-formed result (formatted output, a no-change signal, or a Format_Error) without aborting via panic.

### Requirement 8: Idempotence and Stability

**User Story:** As a developer, I want formatting to be stable, so that repeated runs and CI checks produce the same result.

#### Acceptance Criteria

1. WHEN the Fluid_Attribute_Wrapping_Plugin formats input that it has already formatted with the same configuration, THE Fluid_Attribute_Wrapping_Plugin SHALL produce output byte-for-byte identical to that input, including whitespace, line endings, and indentation.
2. WHEN the Fluid_Attribute_Wrapping_Plugin formats the same input a second time with the same configuration, THE Fluid_Attribute_Wrapping_Plugin SHALL produce output byte-for-byte identical to the output of the first run.
3. IF a formatting pass produces output that differs from its input when that input was already formatted with the same configuration, THEN THE Fluid_Attribute_Wrapping_Plugin SHALL return a Format_Error indicating that idempotence was not achieved and SHALL leave the original input unchanged.
4. THE Fluid_Attribute_Wrapping_Plugin idempotence guarantee SHALL be limited to the Plugin's own output as an intra-plugin fixed point, and cross-plugin convergence across the dprint formatting loop SHALL be out of scope and dependent on Upstream_Plugins not re-touching Start_Tags the Fluid_Attribute_Wrapping_Plugin has already wrapped.
5. WHEN the Fluid_Attribute_Wrapping_Plugin performs the idempotence self-check, THE Fluid_Attribute_Wrapping_Plugin SHALL use the identical column-width function in both passes of that self-check, so that width estimation cannot differ between passes.

### Requirement 9: Code Quality, Typing, and Testing

**User Story:** As a maintainer, I want the Plugin to be strictly typed, follow current best practices, and be covered by tests, so that it is reliable and maintainable.

#### Acceptance Criteria

1. THE Fluid_Attribute_Wrapping_Plugin SHALL be implemented in Rust using the `dprint-core` crate with its `wasm` feature.
2. WHEN the project's configured lint settings (clippy and rustc) are applied during compilation, THE Fluid_Attribute_Wrapping_Plugin source SHALL compile with zero compiler warnings and zero clippy warnings.
3. THE Fluid_Attribute_Wrapping_Plugin SHALL include automated unit tests covering Fluid_Wrapping, Attribute_Sorting, and configuration resolution.
4. THE Fluid_Attribute_Wrapping_Plugin SHALL include automated tests that verify idempotence of formatting, such that for each supported Markup_Dialect, formatting a representative set of at least 5 Start_Tags a second time produces output byte-identical to the first formatting result.
5. THE Fluid_Attribute_Wrapping_Plugin SHALL include automated tests that verify markup-structure equivalence between input and output for a representative set of at least 5 inputs per supported Markup_Dialect, where equivalence means identical element nesting, identical attribute name-value pairs, and identical text content, disregarding only whitespace introduced or removed by formatting.
6. WHEN the test suite runs in a single execution (non-watch) mode, THE Fluid_Attribute_Wrapping_Plugin test suite SHALL complete and report an aggregate pass or fail result.
7. IF any automated test fails during a single-execution test run, THEN THE Fluid_Attribute_Wrapping_Plugin test suite SHALL report a fail result that identifies each failing test by name.
8. WHEN the Fluid_Attribute_Wrapping_Plugin formats a representative 10,000-line markup file, including both the per-tag structure-equivalence self-check and the idempotence self-check, THE Fluid_Attribute_Wrapping_Plugin SHALL complete the single format pass within 50 milliseconds on typical developer-class hardware and SHALL scan the input in time linear in input size, where the 50-millisecond figure is an indicative target rather than a hard real-time guarantee.
