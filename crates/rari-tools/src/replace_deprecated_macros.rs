use std::borrow::Cow;
use std::collections::HashMap;
use std::{fs, path};

use lazy_static::lazy_static;
use rari_doc::pages::page::Page;
// use rari_doc::resolve::{url_meta_from, UrlMeta};
use rari_types::globals::content_translated_root;
use rari_types::locale::Locale;
use rari_utils::concat_strs;
use regex::RegexBuilder;

use crate::error::ToolError;
use crate::utils::{get_redirects_map, read_all_doc_pages, read_files_parallel};

lazy_static! {
    static ref DOCS: HashMap<(Locale, Cow<'static, str>), Page> =
        read_all_doc_pages().expect("read_all_doc_pages failed");
}

lazy_static! {
    static ref REDIRECT_MAPS: HashMap<Locale, HashMap<String, String>> =
        Locale::for_generic_and_spas()
            .iter()
            .map(|locale| {
                (
                    *locale,
                    get_redirects_map(*locale)
                        .iter()
                        .map(|(k, v)| (k.to_lowercase(), v.to_string()))
                        .collect(),
                )
            })
            .collect();
}

fn read_all_files(locale: Locale) -> Result<Vec<(String, String)>, ToolError> {
    let translated_root = content_translated_root()
        .expect("translated root not set")
        .to_str()
        .unwrap();

    let locale_root = concat_strs!(
        translated_root,
        &path::MAIN_SEPARATOR_STR,
        locale.as_folder_str()
    );

    let files = read_files_parallel(&[locale_root])?;
    let files = files
        .into_iter()
        .filter(|(p, _)| {
            let x = p.replace(translated_root, "");
            let x = x.trim_start_matches(path::MAIN_SEPARATOR).to_lowercase();
            x.starts_with(locale.as_folder_str())
        })
        .collect::<Vec<_>>();
    Ok(files)
}

pub fn replace_deprecated_macros(locale: Locale) -> Result<(), ToolError> {
    let files = read_all_files(locale)?;

    let event_re = RegexBuilder::new(r"\{\{\s*event([^}]*)\s*\}\}")
        .case_insensitive(true)
        .build()
        .unwrap();

    let no_tag_omission_re = RegexBuilder::new(r"\{\{\s*no_tag_omission\s*\}\}")
        .case_insensitive(true)
        .build()
        .unwrap();

    let page_re = RegexBuilder::new(r"\{\{\s*page(\([^}]*)\s*\}\}")
        .case_insensitive(true)
        .build()
        .unwrap();

    let todo_re = RegexBuilder::new(r"\{\{\s*todo[^}]*\s*\}\}")
        .case_insensitive(true)
        .build()
        .unwrap();

    let mut count = 0;
    files.iter().for_each(|(path, content)| {
        let result = event_re.replace_all(content, |caps: &regex::Captures| {
            process_event_macro(locale, caps)
        });
        let result = no_tag_omission_re.replace_all(&result, |caps: &regex::Captures| {
            process_no_tag_omission_macro(locale, caps)
        });
        let result = page_re.replace_all(&result, |caps: &regex::Captures| {
            process_page_macro(locale, caps)
        });
        let result = todo_re.replace_all(&result, |caps: &regex::Captures| {
            process_todo_macro(locale, caps)
        });
        if result != *content {
            fs::write(path, &*result).expect("could not write file");
            count += 1;
        }
    });
    println!("Changed {} files", count);

    Ok(())
}

fn process_todo_macro(_locale: Locale, _caps: &regex::Captures) -> String {
    "<! TODO: add content -->".to_string()
}

fn process_page_macro(_locale: Locale, caps: &regex::Captures) -> String {
    let args = collect_args(caps);
    // println!("cap: {} args: {:#?}", caps.get(0).unwrap().as_str(), args);
    if let Some(section) = args.get(1) {
        match section.to_lowercase().as_str() {
            "specifications" => return "{{Specifications}}".to_string(),
            "browser_compatibility" => return "{{Compat}}".to_string(),
            _ => {
                return format!(
                    "<!-- TODO: page macro not supported: {} -->",
                    caps.get(0)
                        .unwrap()
                        .as_str()
                        .replace("{", "")
                        .replace("}", "")
                )
            }
        }
    }
    // caps.get(0).unwrap().as_str().to_string()
    String::default()
}

fn process_no_tag_omission_macro(locale: Locale, _caps: &regex::Captures) -> String {
    match locale {
        Locale::EnUs => "None, both the starting and ending tag are mandatory.".to_string(),
        Locale::De => {
            "Keine, sowohl das Start- als auch das End-Tag sind obligatorisch.".to_string()
        }
        Locale::Fr => {
            "Aucune, la balise d'ouverture et la balise de fermeture sont obligatoires.".to_string()
        }
        Locale::Es => {
            "Ninguna, tanto la etiqueta inicial como la final son obligatorias.".to_string()
        }
        Locale::Ja => "不可。開始と終了タグの両方が必要。".to_string(),
        Locale::Ko => "불가능, 시작과 끝에 태그를 추가하는것은 필수입니다.".to_string(),
        Locale::PtBr => "Nenhuma, tanto a tag inicial quanto a final são obrigatórias.".to_string(),
        Locale::ZhCn => "不允许，开始标签和结束标签都不能省略。".to_string(),
        Locale::Ru => "Нет, открывающий и закрывающий теги обязательны.".to_string(),
        Locale::ZhTw => "不允许，开始标签和结束标签都不能省略。".to_string(),
    }
}

fn process_event_macro(locale: Locale, caps: &regex::Captures) -> String {
    let args = collect_args(caps);

    let event = *args
        .first()
        .expect("Could not get first argument for event macro");
    let mut link_text = args.get(1).unwrap_or(&event).to_string();
    let mut anchor = args.get(2).unwrap_or(&"").to_string();
    let url = concat_strs!("/", locale.as_url_str(), "/docs/Web/Events/", event);

    if !anchor.is_empty() {
        link_text = concat_strs!(&link_text, ".", &anchor);
        anchor = concat_strs!("#", &anchor);
    }

    let url = REDIRECT_MAPS
        .get(&locale)
        .expect("Redirect map for locale not loaded")
        .get(&url.to_lowercase())
        .unwrap_or(&url);

    // let UrlMeta {
    //     slug,
    //     folder_path: _,
    //     locale: _,
    //     page_category: _,
    // } = url_meta_from(&url).unwrap();
    // println!("url meta slug: {:#?}", slug);
    // let doc_exists = DOCS
    //     .get(&(locale, std::borrow::Cow::Borrowed(slug)))
    //     .is_some();
    // If the target does not exist, check local redirects, the en-us redirects
    // let url = if !doc_exists {
    //     REDIRECT_MAPS
    //         .get(&locale)
    //         .expect("Redirect map for locale not loaded")
    //         .get(&url.to_lowercase())
    //         .unwrap_or_else(|| {
    //             let en_us_url_lc =
    //                 concat_strs!("/", Locale::EnUs.as_folder_str(), "/docs/", slug).to_lowercase();
    //             REDIRECT_MAPS
    //                 .get(&Locale::EnUs)
    //                 .expect("Redirect map for locale en-us not loaded")
    //                 .get(&en_us_url_lc)
    //                 .unwrap_or(&url)
    //         })
    // } else {
    //     &url
    // };

    // println!("resolved url: {url}");

    format!("[`{}`]({}{})", link_text, url, anchor)
}

fn collect_args<'a>(caps: &'a regex::Captures<'a>) -> Vec<&'a str> {
    let args = caps
        .get(1)
        .unwrap()
        .as_str()
        .trim()
        .trim_end_matches(')')
        .trim_start_matches('(')
        // double (( and )) are a thing, appearantly
        .trim_end_matches(')')
        .trim_start_matches('(')
        .split(',')
        .map(|a| {
            a.trim()
                .trim_start_matches('\'')
                .trim_end_matches('\'')
                .trim_start_matches('"')
                .trim_end_matches('"')
        })
        .collect::<Vec<_>>();
    args
}
