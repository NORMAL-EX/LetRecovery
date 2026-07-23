//! Pure display-name normalization helpers for hardware information.

/// 检查字符串是否为占位符值（如 "To Be Filled", "Default string" 等）。
pub fn is_placeholder_str(value: &str) -> bool {
    let lower = value.to_lowercase();
    lower.contains("to be filled")
        || lower.contains("default string")
        || lower == "none"
        || lower == "n/a"
        || lower == "unknown"
        || lower.is_empty()
}

pub(super) fn is_placeholder(value: &str) -> bool {
    is_placeholder_str(value)
}

pub fn beautify_manufacturer_name(name: &str) -> String {
    let name_lower = name.to_lowercase();
    if name_lower.contains("asus") || name_lower.contains("asustek") {
        return "华硕电脑".to_string();
    }
    if name_lower.contains("lenovo") {
        return "联想".to_string();
    }
    if name_lower.contains("dell") {
        return "戴尔".to_string();
    }
    if name_lower.contains("hp") || name_lower.contains("hewlett") {
        return "惠普".to_string();
    }
    if name_lower.contains("acer") {
        return "宏碁".to_string();
    }
    if name_lower.contains("msi") || name_lower.contains("micro-star") {
        return "微星".to_string();
    }
    if name_lower.contains("gigabyte") {
        return "技嘉".to_string();
    }
    if name_lower.contains("huawei") {
        return "华为".to_string();
    }
    if name_lower.contains("xiaomi") {
        return "小米".to_string();
    }
    if name_lower.contains("honor") {
        return "荣耀".to_string();
    }
    if name_lower.contains("samsung") {
        return "三星".to_string();
    }
    if name_lower.contains("apple") {
        return "苹果".to_string();
    }
    if name_lower.contains("microsoft") {
        return "微软".to_string();
    }
    if name_lower.contains("razer") {
        return "雷蛇".to_string();
    }
    if name_lower.contains("alienware") {
        return "外星人".to_string();
    }
    name.to_string()
}

pub fn beautify_memory_manufacturer(name: &str) -> String {
    let name_lower = name.to_lowercase();
    if name_lower.contains("micron") {
        return "镁光".to_string();
    }
    if name_lower.contains("samsung") {
        return "三星".to_string();
    }
    if name_lower.contains("hynix") {
        return "海力士".to_string();
    }
    if name_lower.contains("kingston") {
        return "金士顿".to_string();
    }
    if name_lower.contains("corsair") {
        return "海盗船".to_string();
    }
    if name_lower.contains("g.skill") || name_lower.contains("gskill") {
        return "芝奇".to_string();
    }
    if name_lower.contains("crucial") {
        return "英睿达".to_string();
    }
    if name_lower.contains("adata") {
        return "威刚".to_string();
    }
    if name.is_empty() || is_placeholder(name) {
        return "未知".to_string();
    }
    name.to_string()
}

pub fn beautify_gpu_name(name: &str) -> String {
    let mut result = name.to_string();
    if result.to_lowercase().contains("nvidia") {
        result = result
            .replace("NVIDIA", "英伟达")
            .replace("nvidia", "英伟达");
    }
    if result.to_lowercase().contains("intel") && !result.contains("英特尔") {
        result = result.replace("Intel", "英特尔").replace("intel", "英特尔");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_common_firmware_placeholders() {
        assert!(is_placeholder_str("To Be Filled By O.E.M."));
        assert!(is_placeholder_str("DEFAULT STRING"));
        assert!(is_placeholder_str("unknown"));
        assert!(!is_placeholder_str("Framework"));
    }

    #[test]
    fn normalizes_known_brands_and_preserves_unknown_names() {
        assert_eq!(
            beautify_manufacturer_name("ASUSTeK COMPUTER INC."),
            "华硕电脑"
        );
        assert_eq!(beautify_memory_manufacturer("SK hynix"), "海力士");
        assert_eq!(beautify_memory_manufacturer("Default string"), "未知");
        assert_eq!(beautify_manufacturer_name("Framework"), "Framework");
    }

    #[test]
    fn gpu_normalization_keeps_the_model_suffix() {
        assert_eq!(
            beautify_gpu_name("NVIDIA GeForce RTX 4090"),
            "英伟达 GeForce RTX 4090"
        );
        assert_eq!(beautify_gpu_name("Intel Arc A770"), "英特尔 Arc A770");
    }
}
