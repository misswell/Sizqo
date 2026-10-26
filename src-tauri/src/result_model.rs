//! 统一结果契约（重构方案 §3 §4 §10 §11）。
//!
//! 这张表存在的唯一理由：`success` 一个布尔同时背过「编码成功」「压缩成功」「没必要
//! 再压」「原图已达标」四件事，于是后端说「没收益」、前端读成「完成」、磁盘上什么都没有。
//! 从此状态由三段各自回答一个问题：
//!
//! ```text
//! EncodeVerdict   引擎这一轮交回了什么字节？（引擎自己知道，别人猜不了）
//! Evaluation      这些字节值不值得落盘？（只有 evaluate 一处回答）
//! ResultOutcome   这次任务对用户的最终结论是什么？（落盘之后才允许写）
//! ```
//!
//! 前端不得再从 `success` / `error` 文案反推这三段中的任何一段。

use serde::{Deserialize, Serialize};

/// 引擎交给调用方的**字节属于哪一种事实**。它不回答任务成败。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EncodeVerdict {
    /// 交回的是这次编码的产物，且 `out_type` 说的就是这些字节的格式。
    #[default]
    Candidate,
    /// 交回的是**原图字节本身**：这一轮没有产出任何更优的东西。
    /// 是「没收益」不是「失败」，但绝不是「压缩成功」。
    KeptOriginal,
    /// 连候选都没给：格式不支持、输入无效或解码失败。
    Rejected,
    /// 编码这一步本身失败（含 CLI 报错后回落到原始字节）。
    Failed,
    /// 编码在**两次尝试之间**被取消（§62）。它没有交回候选，也不是失败：
    /// 用户不要了，磁盘上什么都没发生过。
    Cancelled,
}

/// 用户可感知的最终结论。与队列生命周期（Pending/Running/Done/…）是两件事。
///
/// `Committed` 与 `TargetUnmet` 是**唯一两个允许带 outputPath** 的取值（§6）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultOutcome {
    /// 输出真的落盘了，且这次操作有据可查。
    Committed,
    /// 压了，但没有值得提交的收益（压完更大、压完一样大、原图已是最优）。
    SkippedNoBenefit,
    /// 原文件本来就已在用户要的体积之内，根本没进入编码。
    SkippedAlreadyUnderTarget,
    /// 输出落盘了，但没达到用户要求的体积目标。压缩成功，承诺未兑现。
    TargetUnmet,
    /// 用户取消。不是系统故障。
    Cancelled,
    /// 真实的处理失败。
    Failed,
}

impl ResultOutcome {
    /// §6.1 只有这两种结论才允许存在输出文件。
    pub fn carries_output(self) -> bool {
        matches!(self, Self::Committed | Self::TargetUnmet)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Committed => "committed",
            Self::SkippedNoBenefit => "skipped_no_benefit",
            Self::SkippedAlreadyUnderTarget => "skipped_already_under_target",
            Self::TargetUnmet => "target_unmet",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
        }
    }
}

/// 结论的理由。前端只读这个字段，不再解析 `error` 文案（§59）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasonCode {
    // 没有收益
    AlreadyUnderTarget,
    NoSizeReduction,
    OutputLargerThanSource,
    TargetUnmet,
    // 输入与编码
    SourceReadFailed,
    SourceNotFound,
    PermissionDenied,
    SourceChanged,
    UnsupportedFormat,
    InvalidInput,
    DecodeFailed,
    EncodeFailed,
    OutputValidationFailed,
    // 落盘事务
    OutputWriteFailed,
    OutputCollision,
    BackupFailed,
    TransactionFailed,
    HistoryWriteFailed,
    RollbackFailed,
    OutputMissing,
    // 生命周期
    Cancelled,
    Stopped,
    /// 压缩会话还在跑，恢复被闸门挡住（不是失败，也不是用户取消）。
    SessionBusy,
    /// 未被任何生产者分类过的失败。它的存在是为了让「还不知道」显式可见，
    /// 而不是冒充成功。
    #[default]
    Unknown,
}

impl ReasonCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AlreadyUnderTarget => "already_under_target",
            Self::NoSizeReduction => "no_size_reduction",
            Self::OutputLargerThanSource => "output_larger_than_source",
            Self::TargetUnmet => "target_unmet",
            Self::SourceReadFailed => "source_read_failed",
            Self::SourceNotFound => "source_not_found",
            Self::PermissionDenied => "permission_denied",
            Self::SourceChanged => "source_changed",
            Self::UnsupportedFormat => "unsupported_format",
            Self::InvalidInput => "invalid_input",
            Self::DecodeFailed => "decode_failed",
            Self::EncodeFailed => "encode_failed",
            Self::OutputValidationFailed => "output_validation_failed",
            Self::OutputWriteFailed => "output_write_failed",
            Self::OutputCollision => "output_collision",
            Self::BackupFailed => "backup_failed",
            Self::TransactionFailed => "transaction_failed",
            Self::HistoryWriteFailed => "history_write_failed",
            Self::RollbackFailed => "rollback_failed",
            Self::OutputMissing => "output_missing",
            Self::Cancelled => "cancelled",
            Self::Stopped => "stopped",
            Self::SessionBusy => "session_busy",
            Self::Unknown => "unknown",
        }
    }
}

/// §11 Evaluate 的结论：唯一一处回答「这些字节值不值得落盘」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Evaluation {
    /// 值得提交。`target_unmet` 说的是用户要的体积有没有兑现。
    Commit { target_unmet: bool },
    /// 不提交：原图才是更优结果。
    Skip(ReasonCode),
    /// 失败：连候选都没有。
    Fail(ReasonCode),
    /// 编码期间被取消：没有候选，也没有失败（§62）。
    Cancel(ReasonCode),
}

impl Evaluation {
    /// 编码阶段交回的字节，配上体积目标与用户是否明确要求换格式，才构成最终判据。
    ///
    /// 三条不容混淆的规则：
    /// - 显式换格式（§18.2）只看「有没有产出那个格式」，**不看有没有变小**；
    /// - 压缩（§18.1）只有变小才提交，一样大和更大都是跳过；
    /// - 引擎交回原图字节（`KeptOriginal`）时，哪怕用户要的是转换，也不能提交——
    ///   落盘的会是原图的副本，那不是「转换成功」。
    pub fn evaluate(
        verdict: EncodeVerdict,
        engine_reason: ReasonCode,
        engine_success: bool,
        encoded_bytes: usize,
        original_size: u64,
        format_conversion: bool,
        target_met: Option<bool>,
    ) -> Self {
        let unmet = target_met == Some(false);
        match verdict {
            EncodeVerdict::Rejected => Evaluation::Fail(engine_reason),
            EncodeVerdict::Failed => Evaluation::Fail(if engine_reason == ReasonCode::Unknown {
                ReasonCode::EncodeFailed
            } else {
                engine_reason
            }),
            EncodeVerdict::KeptOriginal => Evaluation::Skip(engine_reason),
            // 取消不是"没收益"：后者是一份已经做完的判断，前者是根本没做完。
            EncodeVerdict::Cancelled => Evaluation::Cancel(match engine_reason {
                ReasonCode::Cancelled | ReasonCode::Stopped => engine_reason,
                _ => ReasonCode::Cancelled,
            }),
            EncodeVerdict::Candidate => {
                if !engine_success {
                    // 判据与 success 打架 = 生产者忘了标 verdict。宁可判失败，不许冒充成功。
                    return Evaluation::Fail(ReasonCode::Unknown);
                }
                let encoded = encoded_bytes as u64;
                if format_conversion {
                    return Evaluation::Commit { target_unmet: unmet };
                }
                if encoded == 0 {
                    return Evaluation::Fail(ReasonCode::EncodeFailed);
                }
                if encoded >= original_size {
                    return Evaluation::Skip(if encoded > original_size {
                        ReasonCode::OutputLargerThanSource
                    } else {
                        ReasonCode::NoSizeReduction
                    });
                }
                Evaluation::Commit { target_unmet: unmet }
            }
        }
    }

    /// 结论落到结果契约上的那一对字段。
    pub fn outcome(self) -> (Option<ResultOutcome>, Option<ReasonCode>) {
        match self {
            // Commit 的最终结论要等落盘真的发生，这里不预先承诺。
            Evaluation::Commit { .. } => (None, None),
            Evaluation::Skip(ReasonCode::AlreadyUnderTarget) => (
                Some(ResultOutcome::SkippedAlreadyUnderTarget),
                Some(ReasonCode::AlreadyUnderTarget),
            ),
            Evaluation::Skip(reason) => (Some(ResultOutcome::SkippedNoBenefit), Some(reason)),
            Evaluation::Fail(reason) => (Some(ResultOutcome::Failed), Some(reason)),
            Evaluation::Cancel(reason) => (Some(ResultOutcome::Cancelled), Some(reason)),
        }
    }

    /// §13 Publish：只有文件真的落盘、历史真的记了，才允许问这句话。
    /// 压到了文件却没达目标不是失败，也不是达成 —— 它是有产物、承诺没兑现（§16）。
    pub fn committed(self) -> (ResultOutcome, Option<ReasonCode>) {
        match self {
            Evaluation::Commit { target_unmet: true } => {
                (ResultOutcome::TargetUnmet, Some(ReasonCode::TargetUnmet))
            }
            Evaluation::Commit { .. } => (ResultOutcome::Committed, None),
            // 不提交的评估走不到 Publish；真走到了就沿用它自己的结论，绝不冒充有产物。
            other => match other.outcome() {
                (Some(outcome), reason) => (outcome, reason),
                (None, _) => (ResultOutcome::Failed, Some(ReasonCode::Unknown)),
            },
        }
    }
}

/// §6 §141 发布前的最后一道核对：结论与字段必须自洽。
///
/// 这条 invariant 是整份方案的核心承诺 ——「没有产物就不许显示产物操作」。
/// 违反它的不是用户能看到的措辞，而是后端的 bug，所以它必须硬到能让调用方把结果降级。
pub fn invariant_violation(
    media_kind: &str,
    file: &str,
    outcome: Option<ResultOutcome>,
    output_path: Option<&str>,
) -> Option<String> {
    let Some(outcome) = outcome else {
        // 迁移期允许历史数据里没有 outcome；但新生成的结果必须走完 Publish。
        return Some(format!("{media_kind} {file} 没有结论就被发布了（outcome 未赋值）"));
    };
    match (outcome, output_path) {
        (ResultOutcome::Committed | ResultOutcome::TargetUnmet, None) => Some(format!(
            "{media_kind} {file} 结论是 {}，却没有输出路径",
            outcome.as_str()
        )),
        (ResultOutcome::SkippedNoBenefit | ResultOutcome::SkippedAlreadyUnderTarget | ResultOutcome::Cancelled | ResultOutcome::Failed, Some(path)) => {
            Some(format!(
                "{media_kind} {file} 结论是 {}，却带着输出路径 {path}",
                outcome.as_str()
            ))
        }
        _ => None,
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    /// 只填判据相关的四个输入，其余取"没有目标、不是转换"。
    fn verdict(
        encode: EncodeVerdict,
        reason: ReasonCode,
        success: bool,
        encoded: usize,
        original: u64,
    ) -> Evaluation {
        Evaluation::evaluate(encode, reason, success, encoded, original, false, None)
    }

    #[test]
    fn a_smaller_candidate_is_the_only_kind_of_win_that_reaches_commit() {
        assert_eq!(
            verdict(EncodeVerdict::Candidate, ReasonCode::Unknown, true, 500, 1000),
            Evaluation::Commit { target_unmet: false }
        );
        // Commit 之前不许预先承诺结论：Publish 才是写结论的地方。
        assert_eq!(
            Evaluation::Commit { target_unmet: false }.outcome(),
            (None, None)
        );
        assert_eq!(
            Evaluation::Commit { target_unmet: false }.committed(),
            (ResultOutcome::Committed, None)
        );
    }

    #[test]
    fn a_larger_or_equal_output_is_skipped_and_never_a_product() {
        // 方案 §14：1MB 压成 1.2MB 是"跳过：压完更大"，不是"成功"。
        let larger = verdict(EncodeVerdict::Candidate, ReasonCode::Unknown, true, 1200, 1000);
        assert_eq!(
            larger.outcome(),
            (
                Some(ResultOutcome::SkippedNoBenefit),
                Some(ReasonCode::OutputLargerThanSource)
            )
        );
        // 一样大不是"更大"，但同样没有收益。
        assert_eq!(
            verdict(EncodeVerdict::Candidate, ReasonCode::Unknown, true, 1000, 1000).outcome(),
            (
                Some(ResultOutcome::SkippedNoBenefit),
                Some(ReasonCode::NoSizeReduction)
            )
        );
        assert!(!ResultOutcome::SkippedNoBenefit.carries_output());
    }

    #[test]
    fn an_explicit_conversion_commits_even_when_bigger_but_never_the_original_copy() {
        // §18.2：用户要的是格式，不是"必须更小"。
        assert_eq!(
            Evaluation::evaluate(
                EncodeVerdict::Candidate,
                ReasonCode::Unknown,
                true,
                1200,
                1000,
                true,
                None
            ),
            Evaluation::Commit { target_unmet: false }
        );
        // 但引擎交回原图字节时落盘的是原图副本，那不是"转换成功"。
        assert_eq!(
            verdict(EncodeVerdict::KeptOriginal, ReasonCode::NoSizeReduction, true, 1000, 1000)
                .outcome()
                .0,
            Some(ResultOutcome::SkippedNoBenefit)
        );
    }

    #[test]
    fn already_under_target_is_its_own_conclusion_not_no_benefit() {
        // §15：原图本来就在预算内，压根没进编码 —— 没发生 Commit，就不能说有产物。
        assert_eq!(
            verdict(
                EncodeVerdict::KeptOriginal,
                ReasonCode::AlreadyUnderTarget,
                true,
                2_000_000,
                2_000_000
            )
            .outcome(),
            (
                Some(ResultOutcome::SkippedAlreadyUnderTarget),
                Some(ReasonCode::AlreadyUnderTarget)
            )
        );
    }

    #[test]
    fn a_missed_target_still_has_a_product_and_says_so() {
        // §16：压到 3MB、用户要 2MB —— 成功但没有兑现承诺。
        let evaluation = Evaluation::evaluate(
            EncodeVerdict::Candidate,
            ReasonCode::Unknown,
            true,
            3_000_000,
            10_000_000,
            false,
            Some(false),
        );
        assert_eq!(evaluation, Evaluation::Commit { target_unmet: true });
        assert_eq!(
            evaluation.committed(),
            (ResultOutcome::TargetUnmet, Some(ReasonCode::TargetUnmet))
        );
        assert!(ResultOutcome::TargetUnmet.carries_output());
    }

    #[test]
    fn a_rejection_or_encode_failure_stays_a_failure_even_though_bytes_came_back() {
        // 引擎失败时交回的正是原图字节 —— 旧代码靠这个把失败读成"没收益"。
        assert_eq!(
            verdict(EncodeVerdict::Rejected, ReasonCode::UnsupportedFormat, false, 1000, 1000)
                .outcome(),
            (Some(ResultOutcome::Failed), Some(ReasonCode::UnsupportedFormat))
        );
        assert_eq!(
            verdict(EncodeVerdict::Failed, ReasonCode::DecodeFailed, false, 1000, 1000).outcome(),
            (Some(ResultOutcome::Failed), Some(ReasonCode::DecodeFailed))
        );
    }

    #[test]
    fn a_candidate_that_claims_no_success_is_not_allowed_to_look_like_a_win() {
        // 判据与 success 打架 = 生产者忘了标 verdict。宁可判失败。
        assert_eq!(
            verdict(EncodeVerdict::Candidate, ReasonCode::Unknown, false, 500, 1000).outcome(),
            (Some(ResultOutcome::Failed), Some(ReasonCode::Unknown))
        );
        // 空字节永远不是产物。
        assert_eq!(
            verdict(EncodeVerdict::Candidate, ReasonCode::Unknown, true, 0, 1000).outcome(),
            (Some(ResultOutcome::Failed), Some(ReasonCode::EncodeFailed))
        );
    }

    #[test]
    fn the_invariant_only_lets_committed_conclusions_carry_a_path() {
        assert_eq!(
            invariant_violation("image", "/a.jpg", Some(ResultOutcome::Committed), None)
                .unwrap(),
            "image /a.jpg 结论是 committed，却没有输出路径"
        );
        assert_eq!(
            invariant_violation(
                "image",
                "/a.jpg",
                Some(ResultOutcome::SkippedNoBenefit),
                Some("/a_compressed.jpg")
            )
            .unwrap(),
            "image /a.jpg 结论是 skipped_no_benefit，却带着输出路径 /a_compressed.jpg"
        );
        assert_eq!(
            invariant_violation("image", "/a.jpg", Some(ResultOutcome::Failed), Some("/a.jpg")),
            Some("image /a.jpg 结论是 failed，却带着输出路径 /a.jpg".into())
        );
        assert_eq!(
            invariant_violation("image", "/a.jpg", Some(ResultOutcome::Cancelled), None),
            None
        );
        assert_eq!(
            invariant_violation("image", "/a.jpg", Some(ResultOutcome::Committed), Some("/a.jpg")),
            None
        );
        // 没结论的结果不许悄悄放出去：那正是"编码成功被读成压缩成功"的入口。
        assert!(invariant_violation("image", "/a.jpg", None, None).is_some());
    }

    #[test]
    fn serialized_names_are_the_ones_the_frontend_reads() {
        // 前端只认这套字面量；改名必须同时改 frontend/app.js 的 resolver。
        let encoded = serde_json::to_string(&vec![
            ResultOutcome::SkippedAlreadyUnderTarget,
            ResultOutcome::TargetUnmet,
        ])
        .unwrap();
        assert_eq!(encoded, r#"["skipped_already_under_target","target_unmet"]"#);
        assert_eq!(
            serde_json::to_string(&ReasonCode::OutputLargerThanSource).unwrap(),
            r#""output_larger_than_source""#
        );
        assert_eq!(
            serde_json::from_str::<ResultOutcome>(r#""cancelled""#).unwrap(),
            ResultOutcome::Cancelled
        );
    }
}
