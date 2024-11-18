use std::borrow::Cow;
use std::cmp::max;
use std::collections::{BTreeMap, HashSet};
use std::fmt::Write;
use std::fs;
use std::fs::File;
use std::io::{BufWriter, Write as _};
use std::path::Path;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::{Arc, LazyLock};

use anyhow::{anyhow, Error};
use base64::prelude::{Engine as _, BASE64_STANDARD_NO_PAD};
use clap::{Args, Parser, Subcommand};
use dashmap::DashMap;
use ignore::types::TypesBuilder;
use ignore::WalkBuilder;
use itertools::Itertools;
use jsonpath_lib::Compiled;
use lol_html::{element, rewrite_str, ElementContentHandlers, RewriteStrSettings, Selector};
use prettydiff::{diff_lines, diff_words};
use rayon::prelude::*;
use regex::Regex;
use serde_json::Value;
use sha2::{Digest, Sha256};
use xml::fmt_html;

mod xml;

fn html(body: &str) -> String {
    format!(
        r#"<!doctype html>
<html lang="en" prefix="og: https://ogp.me/ns#">

<head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <style>
        body > ul {{
            & > li {{
                list-style: none;
            }}
        ul {{
            display: flex;
            flex-direction: column;
            & li {{
                margin: 1rem;
                border: 1px solid gray;
                list-style: none;
                display: grid;
                grid-template-areas: "h h" "a b" "r r";
                grid-auto-columns: 1fr 1fr;
                & > span {{
                    padding: .5rem;
                    background-color: lightgray;
                    grid-area: h;
                }}
                & > div {{
                    padding: .5rem;
                    &.a {{
                        grid-area: a;
                    }}
                    &.b {{
                        grid-area: b;
                    }}
                    &.r {{
                        grid-area: r;
                    }}

                    & > pre {{
                        text-wrap: wrap;
                    }}
                }}
            }}
        }}
        }}
    </style>
</head>
<body>
<ul>
{body}
</ul>
</body>
</html>
"#
    )
}

pub(crate) fn walk_builder(path: &Path) -> Result<WalkBuilder, Error> {
    let mut types = TypesBuilder::new();
    types.add_def("json:index.json")?;
    types.select("json");
    let mut builder = ignore::WalkBuilder::new(path);
    builder.types(types.build()?);
    Ok(builder)
}

pub fn gather(path: &Path, selector: Option<&str>) -> Result<BTreeMap<String, Value>, Error> {
    let template = if let Some(selector) = selector {
        Some(Compiled::compile(selector).map_err(|e| anyhow!("{e}"))?)
    } else {
        None
    };
    walk_builder(path)?
        .build()
        .filter_map(Result::ok)
        .filter(|f| f.file_type().map(|ft| ft.is_file()).unwrap_or(false))
        .map(|p| {
            let json_str = fs::read_to_string(p.path())?;
            let index: Value = serde_json::from_str(&json_str)?;

            let extract = if let Some(template) = &template {
                template
                    .select(&index)
                    .unwrap_or_default()
                    .into_iter()
                    .next()
                    .cloned()
                    .unwrap_or(Value::Null)
            } else {
                index
            };
            Ok::<_, Error>((p.path().strip_prefix(path)?.display().to_string(), extract))
        })
        .collect()
}

use std::path::PathBuf;

#[derive(Parser)]
#[command(version, about, long_about = None)]
#[command(propagate_version = true)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Diff(BuildArgs),
}
#[derive(Args)]
struct BuildArgs {
    #[arg(short, long)]
    query: Option<String>,
    #[arg(short, long)]
    out: PathBuf,
    root_a: PathBuf,
    root_b: PathBuf,
    #[arg(long)]
    html: bool,
    #[arg(long)]
    csv: bool,
    #[arg(long)]
    inline: bool,
    #[arg(long)]
    ignore_html_whitespace: bool,
    #[arg(long)]
    fast: bool,
    #[arg(long)]
    value: bool,
    #[arg(short, long)]
    verbose: bool,
    #[arg(long)]
    sidebars: bool,
    #[arg(long)]
    check_dts: bool,
    #[arg(long)]
    ignore_ps: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PathIndex {
    Object(String),
    Array(usize),
}

fn make_key(path: &[PathIndex]) -> String {
    path.iter()
        .map(|k| match k {
            PathIndex::Object(s) => s.to_owned(),
            PathIndex::Array(i) => i.to_string(),
        })
        .join(".")
}

fn is_html(s: &str) -> bool {
    s.trim_start().starts_with('<') && s.trim_end().ends_with('>')
}

const IGNORED_KEYS: &[&str] = &[
    "doc.flaws",
    "blogMeta.readTime",
    "doc.modified",
    "doc.popularity",
    "doc.source.github_url",
    "doc.source.last_commit_url",
    //"doc.sidebarHTML",
    "doc.sidebarMacro",
    "doc.hasMathML",
    "doc.other_translations",
    "doc.summary",
];

static SKIP_GLOB_LIST: LazyLock<Vec<&str>> = LazyLock::new(|| {
    vec![
        "docs/mdn/writing_guidelines/",
        "docs/mozilla/add-ons/webextensions/",
        "docs/mozilla/firefox/releases/",
    ]
});

static ALLOWLIST: LazyLock<HashSet<(&str, &str)>> = LazyLock::new(|| {
    vec![
        // Wrong auto-linking of example.com properly escaped link, unfixable in yari
        ("docs/glossary/http/index.json", "doc.body.0.value.content"),
        ("docs/learn/html/multimedia_and_embedding/other_embedding_technologies/index.json", "doc.body.4.value.content"),
        ("docs/web/http/headers/content-security-policy/frame-ancestors/index.json", "doc.body.2.value.content"),
        // Relative link to MDN Playground gets rendered as dead link in yari, correct in rari
        ("docs/learn/learning_and_getting_help/index.json", "doc.body.3.value.content"),
        ("docs/web/media/formats/video_codecs/index.json", "doc.body.6.value.content"),
        // 'unsupported templ: livesamplelink' in rari, remove when supported
        ("docs/learn/forms/form_validation/index.json", "doc.body.12.value.content"),
        ("docs/mdn/writing_guidelines/page_structures/live_samples/index.json", "doc.body.9.value.content"),
        ("docs/web/html/element/img/index.json", "doc.body.15.value.content"),
        ("docs/web/css/_colon_-moz-window-inactive/index.json", "doc.body.5.value.content"),
        // p tag removal in lists
        ("docs/learn/server-side/express_nodejs/deployment/index.json", "doc.body.11.value.content"),
        ("docs/web/http/headers/set-login/index.json", "doc.body.2.value.content"),
        ("docs/web/html/element/a/index.json", "doc.body.2.value.content"),
        ("docs/web/css/background-size/index.json", "doc.body.4.value.content"),
        ("docs/web/css/color_value/color-mix/index.json", "doc.body.2.value.content"),
        // link element re-structure, better in rari
        ("docs/learn/common_questions/design_and_accessibility/design_for_all_types_of_users/index.json", "doc.body.5.value.content"),
        ("docs/learn/html/multimedia_and_embedding/video_and_audio_content/index.json", "doc.body.2.value.content"),
        ("docs/web/http/headers/content-security-policy/sources/index.json", "doc.body.1.value.content"),
        // id changes, no problem
        ("docs/learn/css/howto/css_faq/index.json", "doc.body.11.value.id"),
        ("docs/learn/forms/property_compatibility_table_for_form_controls/index.json", "doc.body.2.value.content"),
        ("docs/learn/html/howto/define_terms_with_html/index.json", "doc.body.0.value.content"),
        ("docs/learn/tools_and_testing/client-side_javascript_frameworks/react_interactivity_filtering_conditional_rendering/index.json", "doc.toc.3.id"),
        ("docs/learn/tools_and_testing/client-side_javascript_frameworks/react_interactivity_filtering_conditional_rendering/index.json", "doc.body.4.value.id"),
        ("docs/mdn/mdn_product_advisory_board/index.json", "doc.body.1.value.content"),
        ("docs/mdn/writing_guidelines/page_structures/live_samples/index.json", "doc.body.11.value.content"),
        ("docs/mdn/writing_guidelines/page_structures/live_samples/index.json", "doc.body.12.value.content"),
        ("docs/mdn/writing_guidelines/page_structures/live_samples/index.json", "doc.body.3.value.content"),
        ("docs/web/performance/speculative_loading/index.json", "doc.body.9.value.id"),
        ("docs/web/manifest/shortcuts/index.json", "doc.toc.1.id"),
        ("docs/web/svg/attribute/preserveaspectratio/index.json", "doc.body.3.value.id"),
        ("docs/web/svg/attribute/preserveaspectratio/index.json", "doc.body.4.value.id"),
        ("docs/web/svg/attribute/preserveaspectratio/index.json", "doc.body.5.value.id"),
        ("docs/web/svg/attribute/preserveaspectratio/index.json", "doc.body.6.value.id"),
        ("docs/web/mathml/element/mfenced/index.json", "doc.body.4.value.content"),
        ("docs/web/javascript/reference/classes/index.json", "doc.body.3.value.content"),
        ("docs/web/javascript/reference/operators/nullish_coalescing/index.json", "doc.body.8.value.id"),
        ("docs/web/css/css_containment/container_size_and_style_queries/index.json", "doc.toc.0.id"),
        ("docs/web/css/css_containment/container_size_and_style_queries/index.json", "doc.toc.2.id"),
        ("docs/web/css/@property/syntax/index.json", "doc.body.4.value.content"),
        ("docs/web/css/css_nesting/nesting_and_specificity/index.json", "doc.body.1.value.id"),
        ("docs/web/css/css_scroll_snap/using_scroll_snap_events/index.json", "doc.body.10.value.content"),
        ("docs/web/css/css_nesting/nesting_and_specificity/index.json", "doc.toc.0.id"),
        ("docs/web/css/nesting_selector/index.json", "doc.body.2.value.id"),
        ("docs/web/css/offset/index.json", "doc.body.5.value.content"),
        ("docs/web/css/position-try-fallbacks/index.json", "doc.body.7.value.content"),
        ("docs/web/css/position-try/index.json", "doc.body.5.value.content"),
        ("docs/web/css/position-area_value/index.json", "doc.toc.5.id"),
        // absolute to relative link change, no problem
        ("docs/learn/forms/styling_web_forms/index.json", "doc.body.10.value.content"),
        ("docs/mdn/kitchensink/index.json", "doc.body.24.value.content"),
        ("docs/web/svg/attribute/begin/index.json", "doc.body.3.value.content"),
        ("docs/web/svg/attribute/begin/index.json", "doc.body.4.value.content"),
        ("docs/web/svg/attribute/begin/index.json", "doc.body.5.value.content"),
        ("docs/web/svg/attribute/begin/index.json", "doc.body.6.value.content"),
        ("docs/web/svg/attribute/begin/index.json", "doc.body.7.value.content"),
        ("docs/web/javascript/reference/global_objects/set/index.json", "doc.body.4.value.content"),
        // encoding changes, no problem
        ("docs/learn/html/introduction_to_html/html_text_fundamentals/index.json", "doc.body.15.value.content"),
        ("docs/learn/tools_and_testing/client-side_javascript_frameworks/vue_computed_properties/index.json", "doc.body.1.value.content"),
        ("docs/learn/tools_and_testing/client-side_javascript_frameworks/react_interactivity_filtering_conditional_rendering/index.json", "doc.body.4.value.i"),
        ("docs/mdn/writing_guidelines/page_structures/links/index.json", "doc.body.3.value.content"),
        ("docs/mdn/writing_guidelines/page_structures/links/index.json", "doc.body.4.value.content"),
        ("docs/mdn/writing_guidelines/page_structures/macros/commonly_used_macros/index.json", "doc.body.14.value.content"),
        ("docs/web/svg/attribute/d/index.json", "doc.body.7.value.content"),
        ("docs/web/svg/attribute/d/index.json", "doc.body.8.value.content"),
        // internal linking fixed in rari
        ("docs/mdn/community/discussions/index.json", "doc.body.0.value.content"),
        ("docs/web/http/headers/te/index.json", "doc.body.1.value.content"),
        // baseline/specification change no problem
        ("docs/mdn/kitchensink/index.json", "doc.baseline"),
        ("docs/mdn/writing_guidelines/page_structures/compatibility_tables/index.json", "doc.baseline"),
        ("docs/web/svg/attribute/data-_star_/index.json", "doc.body.1.value.specifications.0"),
        ("docs/web/http/headers/permissions-policy/screen-wake-lock/index.json", "doc.baseline"),
        ("docs/web/mathml/global_attributes/mathbackground/index.json", "doc.body.3.value.content"),
        ("docs/web/mathml/global_attributes/mathbackground/index.json", "doc.body.3.value.content"),
        ("docs/web/mathml/global_attributes/mathbackground/index.json", "doc.body.4.type"),
        ("docs/web/mathml/global_attributes/mathbackground/index.json", "doc.body.4.value.content"),
        ("docs/web/mathml/global_attributes/mathbackground/index.json", "doc.body.4.value.id"),
        ("docs/web/mathml/global_attributes/mathbackground/index.json", "doc.body.4.value.query"),
        ("docs/web/mathml/global_attributes/mathbackground/index.json", "doc.body.4.value.title"),
        ("docs/web/mathml/global_attributes/mathbackground/index.json", "doc.body.5.type"),
        ("docs/web/mathml/global_attributes/mathbackground/index.json", "doc.body.5.value.content"),
        ("docs/web/mathml/global_attributes/mathbackground/index.json", "doc.body.5.value.id"),
        ("docs/web/mathml/global_attributes/mathbackground/index.json", "doc.body.5.value.query"),
        ("docs/web/mathml/global_attributes/mathbackground/index.json", "doc.body.5.value.title"),
        ("docs/web/mathml/global_attributes/mathbackground/index.json", "doc.body.6"),
        ("docs/web/mathml/global_attributes/mathcolor/index.json", "doc.body.3.value.content"),
        ("docs/web/mathml/global_attributes/mathcolor/index.json", "doc.body.4.type"),
        ("docs/web/mathml/global_attributes/mathcolor/index.json", "doc.body.4.value.content"),
        ("docs/web/mathml/global_attributes/mathcolor/index.json", "doc.body.4.value.id"),
        ("docs/web/mathml/global_attributes/mathcolor/index.json", "doc.body.4.value.query"),
        ("docs/web/mathml/global_attributes/mathcolor/index.json", "doc.body.4.value.title"),
        ("docs/web/mathml/global_attributes/mathcolor/index.json", "doc.body.5.type"),
        ("docs/web/mathml/global_attributes/mathcolor/index.json", "doc.body.5.value.content"),
        ("docs/web/mathml/global_attributes/mathcolor/index.json", "doc.body.5.value.id"),
        ("docs/web/mathml/global_attributes/mathcolor/index.json", "doc.body.5.value.query"),
        ("docs/web/mathml/global_attributes/mathcolor/index.json", "doc.body.5.value.title"),
        ("docs/web/mathml/global_attributes/mathcolor/index.json", "doc.body.6"),
        ("docs/web/mathml/global_attributes/mathsize/index.json", "doc.body.3.value.content"),
        ("docs/web/mathml/global_attributes/mathsize/index.json", "doc.body.4.type"),
        ("docs/web/mathml/global_attributes/mathsize/index.json", "doc.body.4.value.content"),
        ("docs/web/mathml/global_attributes/mathsize/index.json", "doc.body.4.value.id"),
        ("docs/web/mathml/global_attributes/mathsize/index.json", "doc.body.4.value.query"),
        ("docs/web/mathml/global_attributes/mathsize/index.json", "doc.body.4.value.title"),
        ("docs/web/mathml/global_attributes/mathsize/index.json", "doc.body.5.type"),
        ("docs/web/mathml/global_attributes/mathsize/index.json", "doc.body.5.value.content"),
        ("docs/web/mathml/global_attributes/mathsize/index.json", "doc.body.5.value.id"),
        ("docs/web/mathml/global_attributes/mathsize/index.json", "doc.body.5.value.query"),
        ("docs/web/mathml/global_attributes/mathsize/index.json", "doc.body.5.value.title"),
        ("docs/web/mathml/global_attributes/mathsize/index.json", "doc.body.6"),
        ("docs/web/css/color_value/hwb/index.json", "doc.baseline"),
        ("docs/web/css/@media/dynamic-range/index.json", "doc.baseline"),
        ("docs/web/css/color_value/hwb/index.json", "doc.baseline"),
        ("docs/web/css/cursor/index.json", "doc.body.5.value.content"),
        ("docs/web/css/font-stretch/index.json", "doc.body.8.value.content"),
        // whitespace changes no problem
        ("docs/mdn/kitchensink/index.json", "doc.body.23.value.title"),
        ("docs/mdn/writing_guidelines/howto/write_an_api_reference/index.json", "doc.body.8.value.content"),
        ("docs/mdn/writing_guidelines/page_structures/code_examples/index.json", "doc.body.7.value.content"),
        // bug in yari
        ("docs/mdn/writing_guidelines/howto/write_an_api_reference/information_contained_in_a_webidl_file/index.json", "doc.body.23.value.content"),
        ("docs/web/accessibility/aria/attributes/aria-autocomplete/index.json", "doc.body.4.value.specifications.1.bcdSpecificationURL"),
        ("docs/web/accessibility/aria/attributes/aria-autocomplete/index.json", "doc.body.4.value.specifications.2"),
        ("docs/web/accessibility/aria/roles/combobox_role/index.json", "doc.body.5.value.specifications.1.bcdSpecificationURL"),
        ("docs/web/accessibility/aria/roles/combobox_role/index.json", "doc.body.5.value.specifications.2"),
        ("docs/web/security/practical_implementation_guides/tls/index.json", "doc.body.10.value.content"),
        ("docs/web/css/reference/index.json", "doc.body.4.value.content"),
        // improvement by p-tag removal/addition
        ("docs/web/html/element/input/button/index.json", "doc.body.11.value.content"),
        ("docs/web/html/element/input/color/index.json", "doc.body.12.value.content"),
        ("docs/web/html/element/input/date/index.json", "doc.body.16.value.content"),
        ("docs/web/html/element/input/datetime-local/index.json", "doc.body.14.value.content"),
        ("docs/web/html/element/input/email/index.json", "doc.body.22.value.content"),
        ("docs/web/html/element/input/file/index.json", "doc.body.17.value.content"),
        ("docs/web/html/element/input/hidden/index.json", "doc.body.9.value.content"),
        ("docs/web/html/element/input/image/index.json", "doc.body.23.value.content"),
        ("docs/web/html/element/input/month/index.json", "doc.body.20.value.content"),
        ("docs/web/html/element/input/number/index.json", "doc.body.22.value.content"),
        ("docs/web/html/element/input/password/index.json", "doc.body.20.value.content"),
        ("docs/web/html/element/input/radio/index.json", "doc.body.11.value.content"),
        ("docs/web/html/element/input/range/index.json", "doc.body.18.value.content"),
        ("docs/web/html/element/input/search/index.json", "doc.body.27.value.content"),
        ("docs/web/html/element/input/tel/index.json", "doc.body.21.value.content"),
        ("docs/web/html/element/input/text/index.json", "doc.body.22.value.content"),
        ("docs/web/html/element/input/time/index.json", "doc.body.22.value.content"),
        ("docs/web/html/element/input/url/index.json", "doc.body.22.value.content"),
        ("docs/web/html/element/input/week/index.json", "doc.body.17.value.content"),
        ("docs/web/html/element/link/index.json", "doc.body.2.value.content"),
        ("docs/web/html/element/script/index.json", "doc.body.1.value.content"),
        ("docs/web/html/element/input/checkbox/index.json", "doc.body.14.value.content"),
        ("docs/web/html/element/input/reset/index.json", "doc.body.11.value.content"),
        ("docs/web/html/element/input/submit/index.json", "doc.body.16.value.content"),
        ("docs/web/css/-webkit-mask-position-x/index.json", "doc.body.9.value.content"),
        ("docs/web/css/-webkit-mask-position-y/index.json", "doc.body.9.value.content"),
        ("docs/web/css/-webkit-mask-repeat-x/index.json", "doc.body.10.value.content"),
        ("docs/web/css/-webkit-mask-repeat-y/index.json", "doc.body.10.value.content"),
        // image dimension rounding error or similar, ok
        ("docs/web/media/formats/video_concepts/index.json", "doc.body.3.value.content"),
        ("docs/web/svg/tutorial/introduction/index.json", "doc.body.0.value.content"),
        ("docs/web/css/css_flexible_box_layout/basic_concepts_of_flexbox/index.json", "doc.body.2.value.content"),
        ("docs/web/css/css_flexible_box_layout/basic_concepts_of_flexbox/index.json", "doc.body.3.value.content"),
        // rari improvement over yari
        ("docs/web/manifest/index.json", "doc.body.1.value.content"),
        ("docs/web/manifest/orientation/index.json", "doc.body.6.value.content"),
        ("docs/web/css/-moz-orient/index.json", "doc.body.3.value.content"),
        ("docs/web/css/css_namespaces/index.json", "doc.body.0.value.content"),
        ("docs/web/css/shape-outside/index.json", "doc.body.4.value.content"),
        ("docs/web/css/stroke-dasharray/index.json", "doc.body.3.value.content"),
        ("docs/web/css/stroke-dashoffset/index.json", "doc.body.3.value.content"),
        ("docs/web/css/stroke-width/index.json", "doc.body.3.value.content"),
        ("docs/web/css/text-anchor/index.json", "doc.body.3.value.content"),
        ("docs/web/css/text-transform/index.json", "doc.body.15.value.content"), // <-- percent-encoded/plain iframe src param
        ("docs/web/css/x/index.json", "doc.body.3.value.content"),
        ("docs/web/css/y/index.json", "doc.body.3.value.content"),
        // ("docs/web/css/stop-opacity/index.json", "doc.body.3.value.content"),
        ("docs/web/css/stop-opacity/index.json", "doc.body.4.value.content"),
        // attribute order, no problem
        ("docs/web/svg/attribute/end/index.json", "doc.body.1.value.content"),
        ("docs/web/html/element/track/index.json", "doc.body.5.value.content"),
        ("docs/web/html/global_attributes/itemscope/index.json", "doc.body.3.value.content"),
        // added <p> tags, no problem
        ("docs/web/svg/attribute/index.json", "doc.body.30.value.content"),
        ("docs/web/svg/attribute/index.json", "doc.body.31.value.content"),
        ("docs/web/svg/element/animatemotion/index.json", "doc.body.4.value.content"),
        ("docs/web/svg/element/font-face-format/index.json", "doc.body.2.value.content"),
        ("docs/web/svg/element/index.json", "doc.body.18.value.content"),
        ("docs/web/svg/element/index.json", "doc.body.19.value.content"),
        ("docs/web/svg/element/index.json", "doc.body.20.value.content"),
        ("docs/web/svg/element/index.json", "doc.body.21.value.content"),
        ("docs/web/svg/element/index.json", "doc.body.22.value.content"),
        ("docs/web/svg/element/index.json", "doc.body.23.value.content"),
        ("docs/web/svg/element/index.json", "doc.body.24.value.content"),
        ("docs/web/svg/element/index.json", "doc.body.25.value.content"),
        ("docs/web/svg/element/index.json", "doc.body.27.value.content"),
        ("docs/web/svg/element/index.json", "doc.body.28.value.content"),
        ("docs/web/svg/element/index.json", "doc.body.29.value.content"),
        ("docs/web/svg/element/index.json", "doc.body.30.value.content"),
        ("docs/web/svg/element/index.json", "doc.body.31.value.content"),
        ("docs/web/svg/element/index.json", "doc.body.32.value.content"),
        ("docs/web/svg/element/index.json", "doc.body.33.value.content"),
        ("docs/web/svg/element/index.json", "doc.body.34.value.content"),
        ("docs/web/svg/element/index.json", "doc.body.35.value.content"),
        ("docs/web/svg/element/index.json", "doc.body.38.value.content"),
        ("docs/web/svg/element/index.json", "doc.body.39.value.content"),
        ("docs/web/css/@font-face/font-stretch/index.json", "doc.body.7.value.content"),
        ("docs/web/css/@starting-style/index.json", "doc.body.4.value.content"),
        ("docs/web/css/css_multicol_layout/index.json", "doc.body.0.value.content"),
        ("docs/web/css/font-variation-settings/index.json", "doc.body.5.value.content"),
        // outdated and unsupported in rari
        ("docs/web/css/-moz-image-region/index.json", "doc.body.3.value.content"),
        // webref version mismatch
        ("docs/web/css/@import/index.json", "doc.body.3.value.content"),
        ("docs/web/css/@supports/index.json", "doc.body.8.value.content"),
        ("docs/web/css/attr/index.json", "doc.body.4.value.content"),
        ("docs/web/css/calc-size/index.json", "doc.body.8.value.content"),
        ("docs/web/css/calc-sum/index.json", "doc.body.2.value.content"),
        ("docs/web/css/content/index.json", "doc.body.5.value.content"),
        ("docs/web/css/stop-color/index.json", "doc.body.3.value.content"),
        ("docs/web/css/stop-color/index.json", "doc.body.4.value.content"),
        ("docs/web/css/text-decoration-thickness/index.json", "doc.body.10.type"),
        ("docs/web/css/text-decoration-thickness/index.json", "doc.body.10.value.content"),
        ("docs/web/css/text-decoration-thickness/index.json", "doc.body.10.value.id"),
        ("docs/web/css/text-decoration-thickness/index.json", "doc.body.10.value.query"),
        ("docs/web/css/text-decoration-thickness/index.json", "doc.body.10.value.title"),
        ("docs/web/css/text-decoration-thickness/index.json", "doc.body.11"),
        ("docs/web/css/text-decoration-thickness/index.json", "doc.body.8.value.content"),
        ("docs/web/css/text-decoration-thickness/index.json", "doc.body.9.type"),
        ("docs/web/css/text-decoration-thickness/index.json", "doc.body.9.value.content"),
        ("docs/web/css/text-decoration-thickness/index.json", "doc.body.9.value.id"),
        ("docs/web/css/text-decoration-thickness/index.json", "doc.body.9.value.query"),
        ("docs/web/css/text-decoration-thickness/index.json", "doc.body.9.value.title"),
        ("docs/web/css/text-overflow/index.json", "doc.body.10.type"),
        ("docs/web/css/text-overflow/index.json", "doc.body.10.value.content"),
        ("docs/web/css/text-overflow/index.json", "doc.body.10.value.id"),
        ("docs/web/css/text-overflow/index.json", "doc.body.10.value.query"),
        ("docs/web/css/text-overflow/index.json", "doc.body.10.value.title"),
        ("docs/web/css/text-overflow/index.json", "doc.body.11.type"),
        ("docs/web/css/text-overflow/index.json", "doc.body.11.value.content"),
        ("docs/web/css/text-overflow/index.json", "doc.body.11.value.id"),
        ("docs/web/css/text-overflow/index.json", "doc.body.11.value.query"),
        ("docs/web/css/text-overflow/index.json", "doc.body.11.value.title"),
        ("docs/web/css/text-overflow/index.json", "doc.body.12"),
        ("docs/web/css/text-overflow/index.json", "doc.body.9.value.content"),
        ("docs/web/css/outline-color/index.json", "doc.body.7.value.content"),
        ("docs/web/css/outline/index.json", "doc.body.8.value.content"),
        ]
    .into_iter()
    .collect()
});

static WS_DIFF: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?<x>>)[\n ]+|[\n ]+(?<y></)"#).unwrap());

static EMPTY_P_DIFF: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"<p>[\n ]*</p>"#).unwrap());

static DIFF_MAP: LazyLock<Arc<DashMap<String, String>>> =
    LazyLock::new(|| Arc::new(DashMap::new()));

/// Run html content through these handlers to clean up the html before minifying and diffing.
fn pre_diff_element_massaging_handlers<'a>(
    args: &BuildArgs,
) -> Vec<(Cow<'a, Selector>, ElementContentHandlers<'a>)> {
    let mut handlers = vec![
        // remove data-flaw-src attributes
        element!("*[data-flaw-src]", |el| {
            el.remove_attribute("data-flaw-src");
            Ok(())
        }),
        element!("*[data-flaw]", |el| {
            el.remove_attribute("data-flaw");
            Ok(())
        }),
        element!("*.page-not-created", |el| {
            el.remove_attribute("class");
            Ok(())
        }),
        // remove ids from notecards, example-headers, code-examples
        element!("div.notecard, div.example-header, div.code-example", |el| {
            el.remove_attribute("id");
            Ok(())
        }),
        // lowercase ids
        element!("*[id]", |el| {
            el.set_attribute(
                "id",
                &el.get_attribute("id").unwrap_or_default().to_lowercase(),
            )?;
            Ok(())
        }),
    ];
    if args.ignore_ps {
        handlers.extend([element!("p", |el| {
            el.remove_and_keep_content();
            Ok(())
        })]);
    }
    if !args.check_dts {
        handlers.extend([
            element!("dt", |el| {
                el.remove_attribute("id");
                Ok(())
            }),
            element!("dt > a[href^=\"#\"", |el| {
                el.remove_and_keep_content();
                Ok(())
            }),
        ]);
    }
    handlers
}

fn full_diff(
    lhs: &Value,
    rhs: &Value,
    file: &str,
    path: &[PathIndex],
    diff: &mut BTreeMap<String, String>,
    args: &BuildArgs,
) {
    if path.len() == 1 {
        if let PathIndex::Object(s) = &path[0] {
            if s == "url" {
                return;
            }
        }
    }
    let key = make_key(path);

    if SKIP_GLOB_LIST.iter().any(|i| file.starts_with(i)) {
        return;
    }

    if ALLOWLIST.contains(&(file, &key)) {
        return;
    }

    if lhs != rhs {
        if IGNORED_KEYS.iter().any(|i| key.starts_with(i))
            || key == "doc.sidebarHTML" && !args.sidebars
        {
            return;
        }
        match (lhs, rhs) {
            (Value::Array(lhs), Value::Array(rhs)) => {
                let len = max(lhs.len(), rhs.len());
                let (lhs, rhs) = if key.ends_with("specifications") {
                    // sort specs by `bcdSpecificationURL` to make the diff more stable
                    // example docs/web/mathml/global_attributes/index.json
                    let mut lhs_sorted = lhs.clone();
                    let mut rhs_sorted = rhs.clone();
                    lhs_sorted.sort_by_key(|v| {
                        v.get("bcdSpecificationURL")
                            .unwrap_or(&Value::Null)
                            .to_string()
                    });
                    rhs_sorted.sort_by_key(|v| {
                        v.get("bcdSpecificationURL")
                            .unwrap_or(&Value::Null)
                            .to_string()
                    });
                    (&lhs_sorted.clone(), &lhs_sorted.clone())
                } else {
                    (lhs, rhs)
                };
                for i in 0..len {
                    let mut path = path.to_vec();
                    path.push(PathIndex::Array(i));
                    full_diff(
                        lhs.get(i).unwrap_or(&Value::Null),
                        rhs.get(i).unwrap_or(&Value::Null),
                        file,
                        &path,
                        diff,
                        args,
                    );
                }
            }
            (Value::Object(lhs), Value::Object(rhs)) => {
                let mut keys: HashSet<&String> = HashSet::from_iter(lhs.keys());
                keys.extend(rhs.keys());
                for key in keys {
                    let mut path = path.to_vec();
                    path.push(PathIndex::Object(key.to_string()));
                    full_diff(
                        lhs.get(key).unwrap_or(&Value::Null),
                        rhs.get(key).unwrap_or(&Value::Null),
                        file,
                        &path,
                        diff,
                        args,
                    );
                }
            }
            (Value::String(lhs), Value::String(rhs)) => {
                let mut lhs = lhs.to_owned();
                let mut rhs = rhs.to_owned();
                match key.as_str() {
                    "doc.sidebarMacro" => {
                        lhs = lhs.to_lowercase();
                        rhs = rhs.to_lowercase();
                    }
                    "doc.summary" => {
                        lhs = lhs.replace("\n  ", "\n");
                        rhs = rhs.replace("\n  ", "\n");
                    }
                    x if x.starts_with("doc.") && x.ends_with("value.id") => {
                        lhs = lhs
                            .trim_end_matches(|c: char| c == '_' || c.is_ascii_digit())
                            .to_string();
                        rhs = rhs
                            .trim_end_matches(|c: char| c == '_' || c.is_ascii_digit())
                            .to_string();
                    }
                    _ => {}
                };
                if is_html(&lhs) && is_html(&rhs) {
                    let lhs_t = WS_DIFF.replace_all(&lhs, "$x$y");
                    let rhs_t = WS_DIFF.replace_all(&rhs, "$x$y");
                    let lhs_t = EMPTY_P_DIFF.replace_all(&lhs_t, "");
                    let rhs_t = EMPTY_P_DIFF.replace_all(&rhs_t, "");
                    let lhs_t = rewrite_str(
                        &lhs_t,
                        RewriteStrSettings {
                            element_content_handlers: pre_diff_element_massaging_handlers(args),
                            ..RewriteStrSettings::new()
                        },
                    )
                    .expect("lolhtml processing failed");
                    let rhs_t = rewrite_str(
                        &rhs_t,
                        RewriteStrSettings {
                            element_content_handlers: pre_diff_element_massaging_handlers(args),
                            ..RewriteStrSettings::new()
                        },
                    )
                    .expect("lolhtml processing failed");
                    lhs = fmt_html(&html_minifier::minify(lhs_t).unwrap());
                    rhs = fmt_html(&html_minifier::minify(rhs_t).unwrap());
                }
                if lhs != rhs {
                    let mut diff_hash = Sha256::new();
                    diff_hash.write_all(lhs.as_bytes()).unwrap();
                    diff_hash.write_all(rhs.as_bytes()).unwrap();
                    let diff_hash = BASE64_STANDARD_NO_PAD.encode(&diff_hash.finalize()[..]);
                    if let Some(hash) = DIFF_MAP.get(&diff_hash) {
                        diff.insert(key, format!("See {}", hash.as_str()));
                        return;
                    }
                    DIFF_MAP.insert(diff_hash, "somewhere else".into());
                    diff.insert(
                        key,
                        ansi_to_html::convert(&if args.fast {
                            diff_lines(&lhs, &rhs).to_string()
                        } else {
                            diff_words(&lhs, &rhs).to_string()
                        })
                        .unwrap(),
                    );
                }
            }
            (lhs, rhs) => {
                let lhs = lhs.to_string();
                let rhs = rhs.to_string();
                if lhs != rhs {
                    diff.insert(
                        key,
                        ansi_to_html::convert(&diff_words(&lhs, &rhs).to_string()).unwrap(),
                    );
                }
            }
        }
    }
}

fn main() -> Result<(), anyhow::Error> {
    let cli = Cli::parse();

    match &cli.command {
        Commands::Diff(arg) => {
            println!("Gathering everything 🧺");
            let start = std::time::Instant::now();
            let a = gather(&arg.root_a, arg.query.as_deref())?;
            let b = gather(&arg.root_b, arg.query.as_deref())?;

            let hits = max(a.len(), b.len());
            let same = AtomicUsize::new(0);
            if arg.html {
                let list_items = a.par_iter().filter_map(|(k, v)| {
                    if b.get(k) == Some(v) {
                        same.fetch_add(1, Relaxed);
                        return None;
                    }

                    if arg.value {
                        let left = v;
                        let right = b.get(k).unwrap_or(&Value::Null);
                        let mut diff = BTreeMap::new();
                        full_diff(left, right, k, &[], &mut diff, arg);
                        if !diff.is_empty() {
                            return Some((k.clone(), format!(
                                r#"<li><span>{k}</span><div class="r"><pre><code>{}</code></pre></div></li>"#,
                                serde_json::to_string_pretty(&diff).unwrap_or_default(),
                            )));
                        } else {
                            same.fetch_add(1, Relaxed);
                        }
                        None
                    } else {
                        let left = &v.as_str().unwrap_or_default();
                        let right = b
                            .get(k)
                            .unwrap_or(&Value::Null)
                            .as_str()
                            .unwrap_or_default();
                        let htmls = if arg.ignore_html_whitespace {
                            let left_html =
                                html_minifier::minify(WS_DIFF.replace_all(left, "$x$y")).unwrap();
                            let right_html =
                                html_minifier::minify(WS_DIFF.replace_all(right, "$x$y")).unwrap();
                            Some((left_html, right_html))
                        } else {
                            None
                        };

                        let (left, right) = htmls
                            .as_ref()
                            .map(|(l, r)| (l.as_str(), r.as_str()))
                            .unwrap_or((left, right));
                        if left == right {
                            println!("only broken links differ");
                            same.fetch_add(1, Relaxed);
                            return None;
                        }
                        if arg.inline {
                            println!("{}", diff_words(left, right));
                        }
                        Some((k.clone(), format!(
                    r#"<li><span>{k}</span><div class="a">{}</div><div class="b">{}</div></li>"#,
                    left, right
                )))
                    }
                }
                ).collect::<Vec<_>>();
                let out: BTreeMap<String, Vec<_>> =
                    list_items
                        .into_iter()
                        .fold(BTreeMap::new(), |mut acc, (k, li)| {
                            let p = k.splitn(4, '/').collect::<Vec<_>>();
                            let cat = match &p[..] {
                                ["docs", "web", cat, ..] => format!("docs/web/{cat}"),
                                ["docs", cat, ..] => format!("docs/{cat}"),
                                [cat, ..] => cat.to_string(),
                                [] => "".to_string(),
                            };
                            acc.entry(cat).or_default().push(li);
                            acc
                        });

                let out = out.into_iter().fold(String::new(), |mut acc, (k, v)| {
                    write!(
                        acc,
                        r#"<li><details><summary>[{}] {k}</summary><ul>{}</ul></details></li>"#,
                        v.len(),
                        v.into_iter().collect::<String>(),
                    )
                    .unwrap();
                    acc
                });
                let file = File::create(&arg.out)?;
                let mut buffer = BufWriter::new(file);

                buffer.write_all(html(&out).as_bytes())?;
            }
            if arg.csv {
                let mut out = Vec::new();
                out.push("File;JSON Path\n".to_string());
                out.extend(
                    a.par_iter()
                        .filter_map(|(k, v)| {
                            if b.get(k) == Some(v) {
                                same.fetch_add(1, Relaxed);
                                return None;
                            }

                            let left = v;
                            let right = b.get(k).unwrap_or(&Value::Null);
                            let mut diff = BTreeMap::new();
                            full_diff(left, right, k, &[], &mut diff, arg);
                            if !diff.is_empty() {
                                return Some(format!(
                                    "{}\n",
                                    diff.into_keys()
                                        .map(|jsonpath| format!("{};{}", k, jsonpath))
                                        .collect::<Vec<_>>()
                                        .join("\n")
                                ));
                            } else {
                                same.fetch_add(1, Relaxed);
                            }
                            None
                        })
                        .collect::<Vec<_>>(),
                );
                let mut file = File::create(&arg.out)?;

                file.write_all(out.into_iter().collect::<String>().as_bytes())?;
            }

            println!(
                "Took: {:?} - {}/{hits} ok, {} remaining",
                start.elapsed(),
                same.load(Relaxed),
                hits - same.load(Relaxed)
            );
        }
    }
    Ok(())
}
