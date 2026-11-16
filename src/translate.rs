use anyhow::{Context, Result};
use serde_json::Value;
use std::time::Duration;

fn google_translate(text: &str) -> Result<String> {
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query.append_pair("client", "gtx");
    query.append_pair("sl", "auto");
    query.append_pair("tl", "ko");
    query.append_pair("dt", "t");
    query.append_pair("q", text);
    let endpoint = format!(
        "https://translate.googleapis.com/translate_a/single?{}",
        query.finish()
    );
    let body = ureq::get(&endpoint)
        .set("User-Agent", "Mozilla/5.0")
        .timeout(Duration::from_secs(30))
        .call()?
        .into_string()?;
    let value: Value =
        serde_json::from_str(&body).context("Google 번역 응답을 JSON으로 읽지 못했습니다")?;
    let parts = value
        .get(0)
        .and_then(Value::as_array)
        .context("Google 번역 결과가 비어 있습니다")?;
    let translated: String = parts
        .iter()
        .filter_map(|part| part.get(0).and_then(Value::as_str))
        .collect();
    anyhow::ensure!(!translated.is_empty(), "Google 번역 결과가 비어 있습니다");
    Ok(translated)
}

fn byte_chunks(text: &str, max_bytes: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    for part in text.split_inclusive(char::is_whitespace) {
        if part.len() > max_bytes {
            if !current.is_empty() {
                chunks.push(std::mem::take(&mut current));
            }
            let mut oversized = String::new();
            for character in part.chars() {
                if oversized.len() + character.len_utf8() > max_bytes {
                    chunks.push(std::mem::take(&mut oversized));
                }
                oversized.push(character);
            }
            if !oversized.is_empty() {
                current = oversized;
            }
        } else if current.len() + part.len() > max_bytes {
            chunks.push(std::mem::take(&mut current));
            current.push_str(part);
        } else {
            current.push_str(part);
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

fn mymemory_translate(text: &str) -> Result<String> {
    let mut translated = Vec::new();
    for chunk in byte_chunks(text, 450) {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        query.append_pair("q", &chunk);
        query.append_pair("langpair", "autodetect|ko");
        let endpoint = format!("https://api.mymemory.translated.net/get?{}", query.finish());
        let body = ureq::get(&endpoint)
            .set("User-Agent", concat!("NSL/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(15))
            .call()?
            .into_string()?;
        let value: Value =
            serde_json::from_str(&body).context("대체 번역 응답을 JSON으로 읽지 못했습니다")?;
        let status = value
            .get("responseStatus")
            .and_then(Value::as_i64)
            .unwrap_or_default();
        anyhow::ensure!(
            status == 200,
            "대체 번역 실패: {}",
            value
                .get("responseDetails")
                .and_then(Value::as_str)
                .unwrap_or("응답 상태 오류")
        );
        let part = value
            .get("responseData")
            .and_then(|data| data.get("translatedText"))
            .and_then(Value::as_str)
            .context("대체 번역 결과가 비어 있습니다")?;
        let part = part
            .replace("&quot;", "\"")
            .replace("&#39;", "'")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&");
        anyhow::ensure!(!part.trim().is_empty(), "대체 번역 결과가 비어 있습니다");
        translated.push(part);
    }
    Ok(translated.join(""))
}

pub fn to_korean(text: &str) -> Result<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut translated = Vec::new();
    for chunk in chars.chunks(4500) {
        let chunk: String = chunk.iter().collect();
        let result = match google_translate(&chunk) {
            Ok(result) => result,
            Err(google_error) => mymemory_translate(&chunk).with_context(|| {
                format!("Google 번역 오류: {google_error}; 대체 번역도 실패했습니다")
            })?,
        };
        translated.push(result);
    }
    Ok(translated.join("\n"))
}
