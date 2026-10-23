//! File types the editor shows in a viewer (3.x `shared/ide-preview.ts`).

fn base_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

fn extension(path: &str) -> String {
    let name = base_name(path);
    match name.rfind('.') {
        Some(dot) if dot > 0 => name[dot + 1..].to_lowercase(),
        _ => String::new(),
    }
}

fn image_type(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "png" => "image/png",
        "apng" => "image/apng",
        "jpg" | "jpeg" | "jfif" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "ico" | "cur" => "image/x-icon",
        "avif" => "image/avif",
        _ => return None,
    })
}

fn audio_type(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" | "oga" | "opus" => "audio/ogg",
        "flac" => "audio/flac",
        "m4a" => "audio/mp4",
        "aac" => "audio/aac",
        "weba" => "audio/webm",
        _ => return None,
    })
}

fn video_type(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "ogv" => "video/ogg",
        "mov" => "video/quicktime",
        "mkv" => "video/x-matroska",
        _ => return None,
    })
}

const WORD: [&str; 4] = ["docx", "docm", "dotx", "dotm"];
const SHEET: [&str; 8] = ["xlsx", "xlsm", "xltx", "xltm", "xlsb", "xls", "xlt", "ods"];
const OFFICE: [&str; 14] = ["pptx", "pptm", "ppsx", "ppsm", "potx", "ppt", "pps", "pot", "odp", "dps", "doc", "dot", "wps", "odt"];

/// The viewer family of a path, or `None` for files the text editor shows.
pub fn kind(path: &str) -> Option<&'static str> {
    let ext = extension(path);
    let ext = ext.as_str();
    if base_name(path).starts_with("~$") && (WORD.contains(&ext) || SHEET.contains(&ext) || OFFICE.contains(&ext) || ext == "rtf") {
        return Some("office-lock");
    }
    Some(match ext {
        "svg" => "svg",
        "md" | "markdown" | "mdown" | "mkd" | "mdx" => "markdown",
        "pdf" => "pdf",
        "lnk" => "shortcut",
        _ if image_type(ext).is_some() => "image",
        _ if audio_type(ext).is_some() => "audio",
        _ if video_type(ext).is_some() => "video",
        _ if WORD.contains(&ext) => "word",
        _ if SHEET.contains(&ext) => "sheet",
        _ if OFFICE.contains(&ext) => "office",
        _ => return None,
    })
}

/// Files whose bytes go to a viewer only: everything with a viewer except SVG and Markdown, which are text.
pub fn is_media(path: &str) -> bool {
    kind(path).is_some_and(|kind| kind != "svg" && kind != "markdown")
}

/// The response type of the byte route.
pub fn media_type(path: &str) -> &'static str {
    let ext = extension(path);
    match ext.as_str() {
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        ext => image_type(ext).or_else(|| audio_type(ext)).or_else(|| video_type(ext)).unwrap_or("application/octet-stream"),
    }
}
