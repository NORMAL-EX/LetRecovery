//! Pure policy for choosing the temporary data volume used by a ViaPE install.
//!
//! This module never probes disks and never executes DiskPart.  Callers provide a fresh inventory
//! from the current boot session, then execute a returned shrink plan only after revalidating the
//! exact target volume and physical disk.

const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;
pub const STAGING_OPERATIONAL_HEADROOM_BYTES: u64 = 2 * GIB;

/// Logical-byte budget for the ViaPE preparation workflow's data volume.
///
/// The caller measures every component from its existing source. A producer such as DISM may
/// legally materialize a different package tree than its read-only Driver Store inventory; after
/// that required producer runs, the caller replaces the provisional component with the observed
/// logical bytes and rechecks the same budget. No component is copied merely to discover its size.
/// The fixed 2 GiB headroom is added once by [`required_staging_bytes`] for filesystem allocation
/// rounding, the bounded handoff log/config, and transactional metadata.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StagingPayloadBudget {
    pub image_bytes: u64,
    pub exported_driver_bytes: u64,
    pub pca_bytes: u64,
    pub user_driver_bytes: u64,
    pub uefiseven_bytes: u64,
    /// Exact bytes of already downloaded, user-selected unattended installers.
    pub preinstalled_software_bytes: u64,
}

impl StagingPayloadBudget {
    pub fn payload_bytes(self) -> Option<u64> {
        self.image_bytes
            .checked_add(self.exported_driver_bytes)?
            .checked_add(self.pca_bytes)?
            .checked_add(self.user_driver_bytes)?
            .checked_add(self.uefiseven_bytes)?
            .checked_add(self.preinstalled_software_bytes)
    }

    pub fn required_bytes(self) -> Option<u64> {
        required_staging_bytes(self.payload_bytes()?)
    }

    /// Remaining free bytes required after `materialized_payload_bytes` from this same budget are
    /// already present on the selected volume. This preserves the one fixed operational headroom
    /// instead of silently consuming it when an authoritative producer exceeds its preflight
    /// inventory.
    pub fn remaining_required_bytes_after(self, materialized_payload_bytes: u64) -> Option<u64> {
        self.required_bytes()?
            .checked_sub(materialized_payload_bytes)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StorageMedia {
    SolidState,
    Rotational,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StorageAttachment {
    Internal,
    External,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StagingCandidate {
    pub letter: char,
    pub disk_number: Option<u32>,
    pub media: StorageMedia,
    pub attachment: StorageAttachment,
    pub free_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShrinkCandidate {
    pub letter: char,
    pub disk_number: Option<u32>,
    pub media: StorageMedia,
    pub attachment: StorageAttachment,
    pub free_bytes: u64,
    /// Set only after the caller confirms NTFS/basic-volume and a stable BitLocker state. Windows
    /// permits shrinking the currently unlocked system volume without first decrypting it.
    pub shrink_is_safe: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StagingPlan {
    Existing {
        letter: char,
        required_bytes: u64,
    },
    ShrinkTarget {
        letter: char,
        size_mb: u64,
        required_bytes: u64,
    },
    Unavailable {
        required_bytes: u64,
    },
}

/// Adds exactly 2 GiB of operational headroom to already-complete payload accounting.
pub fn required_staging_bytes(payload_bytes: u64) -> Option<u64> {
    payload_bytes.checked_add(STAGING_OPERATIONAL_HEADROOM_BYTES)
}

pub fn select_staging_plan(
    payload_bytes: u64,
    target_disk_number: Option<u32>,
    candidates: &[StagingCandidate],
    shrink_target: Option<ShrinkCandidate>,
) -> StagingPlan {
    let Some(required_bytes) = required_staging_bytes(payload_bytes) else {
        return StagingPlan::Unavailable {
            required_bytes: u64::MAX,
        };
    };
    let best_existing = candidates
        .iter()
        .copied()
        .filter(|candidate| candidate_has_room(*candidate, required_bytes))
        .max_by_key(|candidate| candidate_rank(*candidate, target_disk_number, required_bytes));

    let safe_shrink = shrink_target.filter(|target| shrink_has_room(*target, required_bytes));
    let prefer_shrink = match (best_existing, safe_shrink) {
        (None, Some(_)) => true,
        (Some(existing), Some(target)) => should_prefer_fixed_shrink(existing, target),
        _ => false,
    };

    if prefer_shrink {
        let target = safe_shrink.expect("prefer_shrink requires a safe target");
        return StagingPlan::ShrinkTarget {
            letter: target.letter,
            size_mb: required_bytes.div_ceil(MIB),
            required_bytes,
        };
    }

    if let Some(existing) = best_existing {
        return StagingPlan::Existing {
            letter: existing.letter,
            required_bytes,
        };
    }

    if let Some(target) = safe_shrink {
        return StagingPlan::ShrinkTarget {
            letter: target.letter,
            size_mb: required_bytes.div_ceil(MIB),
            required_bytes,
        };
    }

    StagingPlan::Unavailable { required_bytes }
}

fn candidate_has_room(candidate: StagingCandidate, required_bytes: u64) -> bool {
    candidate.free_bytes >= required_bytes
}

fn shrink_has_room(target: ShrinkCandidate, required_bytes: u64) -> bool {
    // QueryMaxReclaimableBytes is only an estimate and Microsoft explicitly documents that it may
    // exceed what Shrink can reclaim. Keeping that estimate in this pure selection data structure
    // invites it to become a false hard gate. Current free space is a necessary capacity bound;
    // the real VDS Shrink call and its post-operation extent readback remain authoritative.
    target.shrink_is_safe && target.free_bytes >= required_bytes
}

fn candidate_rank(
    candidate: StagingCandidate,
    target_disk_number: Option<u32>,
    required_bytes: u64,
) -> (u16, u8, u64, std::cmp::Reverse<char>) {
    let media_score = match (candidate.attachment, candidate.media) {
        (StorageAttachment::Internal, StorageMedia::SolidState) => 600,
        (StorageAttachment::Internal, StorageMedia::Unknown) => 450,
        (StorageAttachment::Internal, StorageMedia::Rotational) => 350,
        (StorageAttachment::Unknown, StorageMedia::SolidState) => 325,
        (StorageAttachment::Unknown, StorageMedia::Unknown) => 275,
        (StorageAttachment::Unknown, StorageMedia::Rotational) => 250,
        (StorageAttachment::External, StorageMedia::SolidState) => 225,
        (StorageAttachment::External, StorageMedia::Unknown) => 175,
        (StorageAttachment::External, StorageMedia::Rotational) => 150,
    };
    let different_disk = u8::from(
        candidate.disk_number.is_some()
            && target_disk_number.is_some()
            && candidate.disk_number != target_disk_number,
    );
    let remaining = candidate.free_bytes.saturating_sub(required_bytes);
    // `max_by_key()` uses the final tuple item as the last tie-breaker. `Reverse` keeps a stable
    // preference for the earlier drive letter without relying on scan order.
    (
        media_score,
        different_disk,
        remaining,
        std::cmp::Reverse(candidate.letter.to_ascii_uppercase()),
    )
}

fn should_prefer_fixed_shrink(existing: StagingCandidate, target: ShrinkCandidate) -> bool {
    // An already suitable fixed/internal volume is preferred regardless of payload size. A former
    // 8-GiB threshold abruptly introduced a destructive Shrink transaction for otherwise identical
    // valid inventories and did not describe any storage-provider capability. Keep only the stable
    // distinction that avoids depending on removable/external media across the PE reboot.
    target.attachment != StorageAttachment::External
        && existing.attachment == StorageAttachment::External
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gib(value: u64) -> u64 {
        value * GIB
    }

    fn candidate(
        letter: char,
        disk_number: u32,
        media: StorageMedia,
        attachment: StorageAttachment,
        free_gib: u64,
    ) -> StagingCandidate {
        StagingCandidate {
            letter,
            disk_number: Some(disk_number),
            media,
            attachment,
            free_bytes: gib(free_gib),
        }
    }

    fn shrink_target(media: StorageMedia, free_gib: u64) -> ShrinkCandidate {
        ShrinkCandidate {
            letter: 'C',
            disk_number: Some(1),
            media,
            attachment: StorageAttachment::Internal,
            free_bytes: gib(free_gib),
            shrink_is_safe: true,
        }
    }

    #[test]
    fn exact_payload_gets_one_fixed_two_gib_headroom() {
        assert_eq!(required_staging_bytes(gib(1)), Some(gib(3)));
        assert_eq!(required_staging_bytes(gib(20)), Some(gib(22)));
        assert_eq!(required_staging_bytes(gib(100)), Some(gib(102)));
        assert_eq!(required_staging_bytes(u64::MAX), None);
    }

    #[test]
    fn component_budget_counts_every_payload_once() {
        let budget = StagingPayloadBudget {
            image_bytes: gib(9),
            exported_driver_bytes: gib(4),
            pca_bytes: 128 * MIB,
            user_driver_bytes: 64 * MIB,
            uefiseven_bytes: 8 * MIB,
            preinstalled_software_bytes: 32 * MIB,
        };
        assert_eq!(budget.payload_bytes(), Some(gib(13) + 232 * MIB));
        assert_eq!(budget.required_bytes(), Some(gib(15) + 232 * MIB));
        assert_eq!(
            budget.remaining_required_bytes_after(gib(4) + 128 * MIB),
            Some(gib(11) + 104 * MIB)
        );
        assert_eq!(budget.remaining_required_bytes_after(gib(16)), None);
    }

    #[test]
    fn existing_internal_ssd_wins_over_a_larger_hdd() {
        let candidates = [
            candidate(
                'D',
                0,
                StorageMedia::Rotational,
                StorageAttachment::Internal,
                500,
            ),
            candidate(
                'E',
                2,
                StorageMedia::SolidState,
                StorageAttachment::Internal,
                40,
            ),
        ];
        assert!(matches!(
            select_staging_plan(gib(6), Some(1), &candidates, None),
            StagingPlan::Existing { letter: 'E', .. }
        ));
    }

    #[test]
    fn small_image_uses_existing_hdd_instead_of_shrinking_system_ssd() {
        let hdd = candidate(
            'D',
            0,
            StorageMedia::Rotational,
            StorageAttachment::Internal,
            100,
        );
        assert!(matches!(
            select_staging_plan(
                gib(6),
                Some(1),
                &[hdd],
                Some(shrink_target(StorageMedia::SolidState, 100))
            ),
            StagingPlan::Existing { letter: 'D', .. }
        ));
    }

    #[test]
    fn large_image_reuses_suitable_internal_hdd_without_a_size_triggered_shrink() {
        let hdd = candidate(
            'D',
            0,
            StorageMedia::Rotational,
            StorageAttachment::Internal,
            100,
        );
        assert!(matches!(
            select_staging_plan(
                gib(20),
                Some(1),
                &[hdd],
                Some(shrink_target(StorageMedia::SolidState, 100))
            ),
            StagingPlan::Existing { letter: 'D', .. }
        ));
    }

    #[test]
    fn fixed_two_gib_headroom_does_not_force_a_size_triggered_shrink() {
        let hdd = candidate(
            'D',
            0,
            StorageMedia::Rotational,
            StorageAttachment::Internal,
            100,
        );
        assert!(matches!(
            select_staging_plan(
                gib(20),
                Some(1),
                &[hdd],
                Some(shrink_target(StorageMedia::SolidState, 40))
            ),
            StagingPlan::Existing { letter: 'D', .. }
        ));
    }

    #[test]
    fn crossing_the_old_eight_gib_boundary_does_not_change_to_destructive_shrink() {
        let hdd = candidate(
            'D',
            0,
            StorageMedia::Rotational,
            StorageAttachment::Internal,
            100,
        );
        let target = Some(shrink_target(StorageMedia::SolidState, 100));
        assert!(matches!(
            select_staging_plan(gib(8) - 1, Some(1), &[hdd], target),
            StagingPlan::Existing { letter: 'D', .. }
        ));
        assert!(matches!(
            select_staging_plan(gib(8), Some(1), &[hdd], target),
            StagingPlan::Existing { letter: 'D', .. }
        ));
    }

    #[test]
    fn caller_marked_unsafe_target_is_never_selected_for_shrink() {
        let mut target = shrink_target(StorageMedia::SolidState, 100);
        target.shrink_is_safe = false;
        assert_eq!(
            select_staging_plan(gib(6), Some(1), &[], Some(target)),
            StagingPlan::Unavailable {
                required_bytes: required_staging_bytes(gib(6)).expect("bounded")
            }
        );
    }

    #[test]
    fn external_disk_is_only_a_fallback() {
        let candidates = [
            candidate(
                'D',
                0,
                StorageMedia::SolidState,
                StorageAttachment::External,
                100,
            ),
            candidate(
                'E',
                2,
                StorageMedia::Rotational,
                StorageAttachment::Internal,
                50,
            ),
        ];
        assert!(matches!(
            select_staging_plan(gib(6), Some(1), &candidates, None),
            StagingPlan::Existing { letter: 'E', .. }
        ));
    }

    #[test]
    fn external_ssd_is_used_when_fixed_storage_lacks_safe_room() {
        let candidates = [
            candidate(
                'D',
                0,
                StorageMedia::Rotational,
                StorageAttachment::Internal,
                8,
            ),
            candidate(
                'U',
                3,
                StorageMedia::SolidState,
                StorageAttachment::External,
                200,
            ),
        ];
        assert!(matches!(
            select_staging_plan(gib(10), Some(1), &candidates, None),
            StagingPlan::Existing { letter: 'U', .. }
        ));
    }

    #[test]
    fn safe_fixed_shrink_wins_over_external_ssd_even_for_a_small_image() {
        let external_ssd = candidate(
            'U',
            3,
            StorageMedia::SolidState,
            StorageAttachment::External,
            200,
        );
        assert!(matches!(
            select_staging_plan(
                gib(4),
                Some(1),
                &[external_ssd],
                Some(shrink_target(StorageMedia::Rotational, 100))
            ),
            StagingPlan::ShrinkTarget { letter: 'C', .. }
        ));
    }

    #[test]
    fn unknown_virtual_disks_choose_the_one_with_more_safe_space() {
        let candidates = [
            candidate(
                'D',
                0,
                StorageMedia::Unknown,
                StorageAttachment::Unknown,
                30,
            ),
            candidate(
                'E',
                2,
                StorageMedia::Unknown,
                StorageAttachment::Unknown,
                80,
            ),
        ];
        assert!(matches!(
            select_staging_plan(gib(6), Some(1), &candidates, None),
            StagingPlan::Existing { letter: 'E', .. }
        ));
    }

    #[test]
    fn fragmented_free_space_is_never_misrepresented_as_one_basic_partition() {
        // Microsoft defines the largest creatable basic partition by the largest contiguous free
        // extent, not by adding unrelated tails. Model the reported extreme layout with 5 GiB on
        // C:, D: and E:. The aggregate 15 GiB exceeds the 12 GiB requirement, but no individual
        // extent can hold it, so the safe result remains unavailable. A future multi-volume
        // carrier must be a separately authenticated design; it must not silently turn disks into
        // dynamic/spanned volumes.
        let candidates = [
            candidate(
                'D',
                1,
                StorageMedia::SolidState,
                StorageAttachment::Internal,
                5,
            ),
            candidate(
                'E',
                1,
                StorageMedia::SolidState,
                StorageAttachment::Internal,
                5,
            ),
        ];
        let c = shrink_target(StorageMedia::SolidState, 5);
        assert_eq!(
            select_staging_plan(gib(10), Some(1), &candidates, Some(c)),
            StagingPlan::Unavailable {
                required_bytes: gib(12)
            }
        );
        assert_eq!(
            candidates
                .iter()
                .map(|candidate| candidate.free_bytes)
                .sum::<u64>()
                + c.free_bytes,
            gib(15)
        );
    }
}
