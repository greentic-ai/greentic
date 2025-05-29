pub fn extension_from_mime(mime: &str) -> &str {
    match mime {
        // Images
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/bmp" => "bmp",
        "image/tiff" => "tiff",
        "image/svg+xml" => "svg",
        "image/x-icon" => "ico",

        // Audio
        "audio/mpeg" => "mp3",
        "audio/wav" => "wav",
        "audio/ogg" => "ogg",
        "audio/webm" => "weba",
        "audio/aac" => "aac",
        "audio/flac" => "flac",

        // Video
        "video/mp4" => "mp4",
        "video/x-msvideo" => "avi",
        "video/x-matroska" => "mkv",
        "video/webm" => "webm",
        "video/quicktime" => "mov",

        // Documents
        "application/pdf" => "pdf",
        "application/msword" => "doc",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => "docx",
        "application/vnd.ms-excel" => "xls",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => "xlsx",
        "application/vnd.ms-powerpoint" => "ppt",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => "pptx",

        // Code / text
        "application/json" => "json",
        "text/plain" => "txt",
        "text/html" => "html",
        "application/xml" | "text/xml" => "xml",
        "text/css" => "css",
        "application/javascript" | "text/javascript" => "js",
        "application/x-yaml" | "text/yaml" => "yaml",
        "application/x-toml" => "toml",

        // Archives
        "application/zip" => "zip",
        "application/gzip" => "gz",
        "application/x-tar" => "tar",
        "application/x-7z-compressed" => "7z",
        "application/x-rar-compressed" => "rar",

        // Fonts
        "font/ttf" => "ttf",
        "font/otf" => "otf",
        "application/font-woff" | "font/woff" => "woff",
        "application/font-woff2" | "font/woff2" => "woff2",

        // Default
        _ => "bin",
    }
}