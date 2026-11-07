//! Guest-only IPC payloads. No host capture, filesystem output, or extra processes.

use crate::adb::{Adb, CmdResult};
use crate::types::{Error, Result};
use sha2::{Digest, Sha256};

const MAX_PNG_BYTES: usize = 64 * 1024 * 1024;
// 64 MiB binary becomes at most 89,478,488 ASCII bytes; metadata is bounded separately.
const MAX_BASE64_BYTES: usize = ((MAX_PNG_BYTES + 2) / 3) * 4;
const MAX_DIMENSION: u32 = 32768;

#[derive(Debug, serde::Serialize)]
pub struct CaptureVmGuestResult {
    pub vm_id: String,
    pub serial: String,
    pub png_base64: String,
    pub width: u32,
    pub height: u32,
    pub sha256: String,
    pub source: &'static str,
    pub captured_at: String,
    pub boot_id: String,
}

/// The command layer keeps the VM's operation lease alive for this whole future.
pub async fn capture(vm_id: String, adb: &Adb) -> Result<CaptureVmGuestResult> {
    let boot_id = current_boot(adb).await?;
    // capture_png already checks signature, every chunk boundary/CRC, IHDR and IEND.
    let png = adb.capture_png().await?;
    if png.len() > MAX_PNG_BYTES {
        return Err(Error::Other("Guest PNG超过64MiB上限".into()));
    }
    let (width, height) = validated_png_dimensions(&png)?;
    let captured_at = crate::now_string();
    require_same_boot(&boot_id, &current_boot(adb).await?)?;
    let sha256 = format!("{:x}", Sha256::digest(&png));
    let png_base64 = encode_base64(&png);
    debug_assert!(png_base64.len() <= MAX_BASE64_BYTES);
    Ok(CaptureVmGuestResult {
        vm_id,
        serial: adb.serial().to_string(),
        png_base64,
        width,
        height,
        sha256,
        source: "guest",
        captured_at,
        boot_id,
    })
}

fn refresh_required(reason: &str) -> Error {
    Error::Other(format!("{reason}；刷新画面后重试"))
}

/// Linux boot_id's canonical lowercase UUID: exactly 8-4-4-4-12 ASCII hex.
fn valid_boot_id(boot_id: &str) -> bool {
    boot_id.len() == 36 && boot_id.bytes().enumerate().all(|(index, byte)| {
        if matches!(index, 8 | 13 | 18 | 23) { byte == b'-' }
        else { byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte) }
    })
}

pub(crate) fn require_same_boot(expected: &str, actual: &str) -> Result<()> {
    if !valid_boot_id(expected) || !valid_boot_id(actual) || expected != actual {
        return Err(refresh_required("Guest启动身份已变化或无效"));
    }
    Ok(())
}

/// Reads only fixed commands. Sandwiching boot_completed detects a reboot mid-query.
pub(crate) async fn current_boot(adb: &Adb) -> Result<String> {
    let before = adb.guest_boot_id().await?;
    if !before.ok {
        return Err(Error::Command(format!("读取Guest boot_id失败: {}；刷新画面后重试", before.output())));
    }
    let boot_id = before.stdout.trim();
    if !valid_boot_id(boot_id) { return Err(refresh_required("Guest boot_id格式无效")); }
    let completed = adb.guest_boot_completed().await?;
    if !completed.ok {
        return Err(Error::Command(format!("读取Guest启动状态失败: {}；刷新画面后重试", completed.output())));
    }
    if completed.stdout.trim() != "1" { return Err(refresh_required("Guest尚未完成启动")); }
    let after = adb.guest_boot_id().await?;
    if !after.ok {
        return Err(Error::Command(format!("复核Guest boot_id失败: {}；刷新画面后重试", after.output())));
    }
    require_same_boot(boot_id, after.stdout.trim())?;
    Ok(boot_id.to_string())
}

/// Run before obtaining any ADB handle, and again at the helper boundary.
pub(crate) fn validate_tap_request(x: u32, y: u32, width: u32, height: u32, boot_id: &str) -> Result<()> {
    if !valid_boot_id(boot_id) { return Err(refresh_required("预期Guest boot_id格式无效")); }
    if !(1..=MAX_DIMENSION).contains(&width) || !(1..=MAX_DIMENSION).contains(&height)
        || x >= width || y >= height {
        return Err(refresh_required("点击坐标或预期画面尺寸无效"));
    }
    Ok(())
}

fn require_frame_match(expected_width: u32, expected_height: u32, actual: (u32, u32)) -> Result<()> {
    if actual != (expected_width, expected_height) {
        return Err(refresh_required("Guest画面尺寸已变化"));
    }
    Ok(())
}

/// User-requested point only. No automatic grant, root command, text, key or gesture.
/// The command layer holds the same manager operation lease throughout every await.
pub async fn tap(adb: &Adb, x: u32, y: u32, expected_width: u32, expected_height: u32, expected_boot_id: &str) -> Result<CmdResult> {
    validate_tap_request(x, y, expected_width, expected_height, expected_boot_id)?;
    require_same_boot(expected_boot_id, &current_boot(adb).await?)?;
    let png = adb.capture_png().await?;
    require_frame_match(expected_width, expected_height, validated_png_dimensions(&png)?)?;
    // Drop a potentially large buffer before the final identity check and one input.
    drop(png);
    require_same_boot(expected_boot_id, &current_boot(adb).await?)?;
    let result = adb.tap_guest(x, y).await?;
    if !tap_command_succeeded(&result) {
        return Err(Error::Command(format!("Guest input tap失败（exit={}）: {}", result.code, result.output())));
    }
    Ok(result)
}

fn tap_command_succeeded(result: &CmdResult) -> bool {
    result.ok && !result.output().to_ascii_lowercase().lines().map(str::trim).any(|line| {
        line.starts_with("error") || line.starts_with("exception") || line.starts_with("usage:")
            || (line.starts_with("java.") && line.contains("exception"))
            || line.contains("permission denial") || line.contains("permission denied")
            || line.contains("invalid arguments")
    })
}

/// O(1) IHDR extraction after capture_png's binary validator, without another CRC pass.
fn validated_png_dimensions(png: &[u8]) -> Result<(u32, u32)> {
    let invalid = || Error::Other("Guest PNG缺少有效尺寸".into());
    let dimensions = png.get(16..24).ok_or_else(invalid)?;
    let width = u32::from_be_bytes(dimensions[..4].try_into().map_err(|_| invalid())?);
    let height = u32::from_be_bytes(dimensions[4..].try_into().map_err(|_| invalid())?);
    if width == 0 || height == 0 || width > 32768 || height > 32768 {
        return Err(invalid());
    }
    Ok((width, height))
}

fn encode_base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::with_capacity(((bytes.len() + 2) / 3) * 4);
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied().unwrap_or(0);
        let third = chunk.get(2).copied().unwrap_or(0);
        encoded.push(TABLE[(first >> 2) as usize] as char);
        encoded.push(TABLE[(((first & 3) << 4) | (second >> 4)) as usize] as char);
        encoded.push(if chunk.len() > 1 { TABLE[(((second & 15) << 2) | (third >> 6)) as usize] as char } else { '=' });
        encoded.push(if chunk.len() > 2 { TABLE[(third & 63) as usize] as char } else { '=' });
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    // Pure fixtures only; execute in cloud CI, not on the operator's desktop.
    #[test]
    fn rfc4648_base64_vectors() {
        for (input, expected) in [
            ("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="), ("fooba", "Zm9vYmE="), ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(encode_base64(input.as_bytes()), expected);
        }
        assert_eq!(encode_base64(&[0, 0xff, 0x80, 0xfd, 0xfe, 0xff]), "AP+A/f7/");
        assert_eq!(MAX_BASE64_BYTES, 89_478_488);
    }

    #[test]
    fn extracts_validated_png_dimensions_without_text_conversion() {
        let mut png = vec![0; 24];
        png[16..20].copy_from_slice(&1080u32.to_be_bytes());
        png[20..24].copy_from_slice(&1920u32.to_be_bytes());
        assert_eq!(validated_png_dimensions(&png).unwrap(), (1080, 1920));
        assert!(validated_png_dimensions(&png[..23]).is_err());
        png[16..20].copy_from_slice(&0u32.to_be_bytes());
        assert!(validated_png_dimensions(&png).is_err());
        png[16..20].copy_from_slice(&32769u32.to_be_bytes());
        assert!(validated_png_dimensions(&png).is_err());
    }

    const BOOT: &str = "12345678-1234-4abc-8def-123456789abc";
    const OTHER_BOOT: &str = "12345678-1234-4abc-8def-123456789abd";

    #[test]
    fn capture_response_has_the_frontend_contract() {
        let result = CaptureVmGuestResult {
            vm_id: "owned-fixture".into(), serial: "emulator-5554".into(),
            png_base64: "AA==".into(), width: 1, height: 2,
            sha256: "a".repeat(64), source: "guest", captured_at: "2026-11-07T00:00:00Z".into(),
            boot_id: BOOT.into(),
        };
        let value = serde_json::to_value(result).unwrap();
        assert_eq!(value.as_object().unwrap().len(), 9);
        assert_eq!(value["source"], "guest");
        assert_eq!(value["width"], 1);
        assert_eq!(value["boot_id"], BOOT);
        assert!(value.get("png_base64").unwrap().is_string());
        assert!(value.get("captured_at").unwrap().is_string());
    }

    #[test]
    fn tap_boot_id_is_canonical_ascii_uuid() {
        assert!(valid_boot_id(BOOT));
        for invalid in ["", "12345678-1234-4ABC-8def-123456789abc", "1234567812344abc8def123456789abc",
            "12345678-1234-4abc-8def-123456789abg", "12345678-1234-4abc-8def-123456789abc\n",
            "12345678-1234-4abc-8def-123456789ab;", " 12345678-1234-4abc-8def-123456789abc",
            "１２３４５６７８-1234-4abc-8def-123456789abc"] {
            assert!(!valid_boot_id(invalid), "{invalid:?}");
        }
    }

    #[test]
    fn tap_coordinates_and_expected_dimensions_are_bounded_before_adb() {
        for (x, y, width, height) in [(0, 0, 1, 1), (32767, 32767, 32768, 32768), (1079, 1919, 1080, 1920)] {
            assert!(validate_tap_request(x, y, width, height, BOOT).is_ok());
        }
        for (x, y, width, height) in [(0, 0, 0, 1), (0, 0, 1, 0), (0, 0, 32769, 1),
            (0, 0, 1, 32769), (1080, 0, 1080, 1920), (0, 1920, 1080, 1920),
            (u32::MAX, 0, 32768, 32768), (0, u32::MAX, 32768, 32768)] {
            let error = validate_tap_request(x, y, width, height, BOOT).unwrap_err().to_string();
            assert!(error.contains("刷新画面后重试"));
        }
        assert!(validate_tap_request(0, 0, 1, 1, "bad boot").is_err());
    }

    #[test]
    fn tap_rejects_changed_boot_or_size_without_any_input() {
        assert!(require_same_boot(BOOT, BOOT).is_ok());
        assert!(require_frame_match(1080, 1920, (1080, 1920)).is_ok());
        for result in [require_same_boot(BOOT, OTHER_BOOT), require_same_boot(BOOT, ""),
            require_frame_match(1080, 1920, (1920, 1080)), require_frame_match(1080, 1920, (0, 1920))] {
            assert!(result.unwrap_err().to_string().contains("刷新画面后重试"));
        }
    }

    #[test]
    fn tap_reports_exit_and_android_text_failures() {
        let result = |ok, stdout: &str, stderr: &str| CmdResult {
            ok, stdout: stdout.into(), stderr: stderr.into(), code: if ok { 0 } else { 1 },
        };
        assert!(tap_command_succeeded(&result(true, "", "")));
        assert!(!tap_command_succeeded(&result(false, "", "")));
        for text in ["Error: bad input", "Exception occurred while executing", "java.lang.SecurityException",
            "Permission Denial: input", "Invalid arguments", "Usage: input [<source>] <command>"] {
            assert!(!tap_command_succeeded(&result(true, text, "")), "{text}");
            assert!(!tap_command_succeeded(&result(true, "", text)), "{text}");
        }
    }
}