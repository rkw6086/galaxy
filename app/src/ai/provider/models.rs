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
