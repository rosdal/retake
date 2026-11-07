//! Checks that an ffmpeg build has the encoders and filters Retake needs.

use std::collections::BTreeSet;

use crate::autoconf::Encoder;

/// Filters every setup needs.
pub const REQUIRED_FILTERS: [&str; 3] = ["ddagrab", "lut3d", "cas"];

/// Encoders and extra filters the chosen encoder needs.
fn required(enc: Encoder) -> (&'static [&'static str], &'static [&'static str]) {
    match enc {
        Encoder::Amf => (&["hevc_amf", "av1_amf", "h264_amf"], &[]),
        Encoder::Qsv => (&["hevc_qsv", "av1_qsv"], &["hwmap"]),
        Encoder::Nvenc => (&["hevc_nvenc", "av1_nvenc"], &[]),
        Encoder::X264 => (&["libx264", "libx265"], &["hwdownload"]),
    }
}

/// Names listed by `ffmpeg -encoders` or `-filters`. A listing line is a column of flag
/// characters followed by the name; legend lines (`V..... = Video`) and headers are skipped.
pub fn parse_names(listing: &str) -> BTreeSet<String> {
    listing
        .lines()
        .filter_map(|line| {
            let mut tokens = line.split_whitespace();
            let flags = tokens.next()?;
            let name = tokens.next()?;
            let flag_like = flags.chars().all(|c| "VASFXBDTCN.|".contains(c));
            (flag_like && name != "=").then(|| name.to_owned())
        })
        .collect()
}

/// Encoders and filters `enc` needs that are absent from the given listings, encoders first.
pub fn missing_features(encoders: &str, filters: &str, enc: Encoder) -> Vec<&'static str> {
    let have_enc = parse_names(encoders);
    let have_fil = parse_names(filters);
    let (encs, extra) = required(enc);
    let missing_enc = encs.iter().filter(|n| !have_enc.contains(**n));
    let missing_fil = REQUIRED_FILTERS
        .iter()
        .chain(extra)
        .filter(|n| !have_fil.contains(**n));
    missing_enc.chain(missing_fil).copied().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::autoconf::Encoder;

    // Real lines from `ffmpeg -hide_banner -encoders` / `-filters` (ffmpeg 8.0.1, gyan full).
    const ENC: &str = "Encoders:
 V..... = Video
 A..... = Audio
 .....D = Supports direct rendering method 1
 ------
 V....D amv                  AMV Video
 V....D av1_amf              AMD AMF AV1 encoder (codec av1)
 V....D hevc_amf             AMD AMF HEVC encoder (codec hevc)
";
    const FIL: &str = "Filters:
  T.. = Timeline support
  | = Source or sink filter
 TS lut3d             V->V       Adjust colors using a 3D LUT.
 .. ddagrab           |->V       Grab Windows Desktop images using Desktop Duplication API
";

    #[test]
    fn reports_missing() {
        assert_eq!(
            missing_features(ENC, FIL, Encoder::Amf),
            ["h264_amf", "cas"]
        );
    }

    #[test]
    fn nothing_missing_when_all_present() {
        let enc = format!("{ENC} V....D h264_amf             AMD AMF H.264 Encoder\n");
        let fil = format!("{FIL} TS cas               V->V       Contrast Adaptive Sharpen.\n");
        assert!(missing_features(&enc, &fil, Encoder::Amf).is_empty());
    }

    #[test]
    fn ignores_header_and_legend_lines() {
        let names = parse_names(ENC);
        assert_eq!(
            names.into_iter().collect::<Vec<_>>(),
            ["amv", "av1_amf", "hevc_amf"]
        );
        assert!(parse_names(FIL).contains("ddagrab"));
        assert!(!parse_names(FIL).contains("="));
    }

    #[test]
    fn empty_output_means_everything_missing() {
        assert_eq!(missing_features("", "", Encoder::Amf).len(), 6);
    }

    #[test]
    fn each_encoder_needs_its_own_codecs() {
        assert_eq!(
            missing_features("", "", Encoder::Qsv),
            ["hevc_qsv", "av1_qsv", "ddagrab", "lut3d", "cas", "hwmap"]
        );
        assert_eq!(
            missing_features("", "", Encoder::Nvenc),
            ["hevc_nvenc", "av1_nvenc", "ddagrab", "lut3d", "cas"]
        );
        let enc = format!(
            "{ENC} V....D hevc_qsv             H.265 (Intel Quick Sync)
 V....D av1_qsv              AV1 (Intel Quick Sync)
"
        );
        let fil = format!(
            "{FIL} TS cas               V->V       x
 .. hwmap             V->V       x
"
        );
        assert!(missing_features(&enc, &fil, Encoder::Qsv).is_empty());
    }
}
