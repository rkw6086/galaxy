pub(crate) fn foundation_model_id(model_id: &str) -> &str {
    let id = model_id.rsplit('/').next().unwrap_or(model_id);
    let id = id
        .strip_suffix("[1m]")
        .or_else(|| id.strip_suffix("[1M]"))
        .unwrap_or(id);
    ["global.", "us.", "eu.", "apac.", "jp.", "au."]
        .iter()
        .find_map(|prefix| id.strip_prefix(prefix))
        .unwrap_or(id)
}
pub fn apply_cross_region_prefix(model_id: &str, region: &str) -> String {
    if model_id.starts_with("arn:") {
        return model_id.to_string();
    }

    if model_id.contains('.') && model_id.split('.').next().unwrap_or("").len() <= 6 {
        return model_id.to_string();
    }

    let prefix = match region {
        r if r.starts_with("us-") || r.starts_with("ca-") => "us",
        r if r.starts_with("eu-") || r == "il-central-1" => "eu",
        r if r == "ap-northeast-1" || r == "ap-northeast-3" => "jp",
        r if r == "ap-southeast-2" || r == "ap-southeast-4" || r == "ap-southeast-6" => "au",
        r if r.starts_with("ap-") => "apac",
        _ => return model_id.to_string(),
    };
    format!("{}.{}", prefix, model_id)
}
