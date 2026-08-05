//! Machine-readable UI error codes for frontend i18n.
//! Format: `i18n:<code>` or `i18n:<code>|key=value|…`

pub fn i18n(code: &str) -> String {
    format!("i18n:{code}")
}

pub fn i18n_kv(code: &str, pairs: &[(&str, String)]) -> String {
    let mut out = format!("i18n:{code}");
    for (k, v) in pairs {
        let safe = v.replace('|', " ").replace('=', ":");
        out.push('|');
        out.push_str(k);
        out.push('=');
        out.push_str(&safe);
    }
    out
}
