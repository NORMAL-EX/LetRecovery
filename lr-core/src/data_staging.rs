//! Pure policy for choosing the temporary data volume used by a ViaPE install.
//!
//! This module never probes disks and never executes DiskPart.  Callers provide a fresh inventory
//! from the current boot session, then execute a returned shrink plan only after revalidating the
//! exact target volume and physical disk.

const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;
const LARGE_IMAGE_SHRINK_THRESHOLD: u64 = 8 * GIB;

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
    pub total_bytes: u64,
    pub is_current_system: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShrinkCandidate {
    pub letter: char,
    pub disk_number: Option<u32>,
    pub media: StorageMedia,
    pub attachment: StorageAttachment,
    pub free_bytes: u64,
    pub total_bytes: u64,
    pub is_current_system: bool,
    pub max_shrink_bytes: u64,
    /// Set only after the caller confirms NTFS/basic-volume and BitLocker safety.
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

/// Includes the image plus bounded room for the PE snapshot, PCA package, drivers, configuration,
/// filesystem allocation rounding, and a partially written destination that must be cleaned up.
pub fn required_staging_bytes(image_bytes: u64) -> u64 {
    let proportional = image_bytes / 10;
    let variable_headroom = proportional.clamp(GIB, 4 * GIB);
    image_bytes
        .saturating_add(variable_headroom)
        .saturating_add(512 * MIB)
}

pub fn select_staging_plan(
    image_bytes: u64,
    target_disk_number: Option<u32>,
    candidates: &[StagingCandidate],
    shrink_target: Option<ShrinkCandidate>,
) -> StagingPlan {
    let required_bytes = required_staging_bytes(image_bytes);
    let best_existing = candidates
        .iter()
        .copied()
        .filter(|candidate| candidate_has_room(*candidate, required_bytes))
        .max_by_key(|candidate| candidate_rank(*candidate, target_disk_number, required_bytes));

    let safe_shrink = shrink_target.filter(|target| shrink_has_room(*target, required_bytes));
    let prefer_shrink = match (best_existing, safe_shrink) {
        (None, Some(_)) => true,
        (Some(existing), Some(target)) => should_prefer_fixed_shrink(image_bytes, existing, target),
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
    candidate.free_bytes
        >= required_bytes.saturating_add(volume_reserve_bytes(
            candidate.is_current_system,
            candidate.total_bytes,
        ))
}

fn shrink_has_room(target: ShrinkCandidate, required_bytes: u64) -> bool {
    target.shrink_is_safe
        && target.max_shrink_bytes >= required_bytes
        && target.free_bytes
            >= required_bytes.saturating_add(volume_reserve_bytes(
                target.is_current_system,
                target.total_bytes,
            ))
}

fn volume_reserve_bytes(is_current_system: bool, total_bytes: u64) -> u64 {
    if is_current_system {
        (total_bytes / 10).max(20 * GIB)
    } else {
        (total_bytes / 50).max(GIB)
    }
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
    let remaining = candidate
        .free_bytes
        .saturating_sub(required_bytes)
        .saturating_sub(volume_reserve_bytes(
            candidate.is_current_system,
            candidate.total_bytes,
        ));
    // `max_by_key()` uses the final tuple item as the last tie-breaker. `Reverse` keeps a stable
    // preference for the earlier drive letter without relying on scan order.
    (
        media_score,
        different_disk,
        remaining,
        std::cmp::Reverse(candidate.letter.to_ascii_uppercase()),
    )
}

fn should_prefer_fixed_shrink(
    image_bytes: u64,
    existing: StagingCandidate,
    target: ShrinkCandidate,
) -> bool {
    target.attachment != StorageAttachment::External
        && (existing.attachment == StorageAttachment::External
            || (image_bytes >= LARGE_IMAGE_SHRINK_THRESHOLD
                && target.media == StorageMedia::SolidState
                && existing.media != StorageMedia::SolidState))
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
            total_bytes: gib(256),
            is_current_system: false,
        }
    }

    fn shrink_target(media: StorageMedia, image_room_gib: u64, free_gib: u64) -> ShrinkCandidate {
        ShrinkCandidate {
            letter: 'C',
            disk_number: Some(1),
            media,
            attachment: StorageAttachment::Internal,
            free_bytes: gib(free_gib),
            total_bytes: gib(256),
            is_current_system: true,
            max_shrink_bytes: gib(image_room_gib),
            shrink_is_safe: true,
        }
    }

    #[test]
    fn headroom_is_bounded_and_grows_with_the_image() {
        assert_eq!(required_staging_bytes(gib(1)), gib(2) + 512 * MIB);
        assert_eq!(required_staging_bytes(gib(20)), gib(22) + 512 * MIB);
        assert_eq!(required_staging_bytes(gib(100)), gib(104) + 512 * MIB);
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
                Some(shrink_target(StorageMedia::SolidState, 60, 100))
            ),
            StagingPlan::Existing { letter: 'D', .. }
        ));
    }

    #[test]
    fn large_image_can_use_abundant_system_ssd_instead_of_hdd() {
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
                Some(shrink_target(StorageMedia::SolidState, 60, 100))
            ),
            StagingPlan::ShrinkTarget {
                letter: 'C',
                size_mb,
                ..
            } if size_mb == (gib(22) + 512 * MIB) / MIB
        ));
    }

    #[test]
    fn system_volume_reserve_prevents_an_aggressive_shrink() {
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
                Some(shrink_target(StorageMedia::SolidState, 60, 40))
            ),
            StagingPlan::Existing { letter: 'D', .. }
        ));
    }

    #[test]
    fn encrypted_or_unknown_target_is_never_selected_for_shrink() {
        let mut target = shrink_target(StorageMedia::SolidState, 60, 100);
        target.shrink_is_safe = false;
        assert_eq!(
            select_staging_plan(gib(6), Some(1), &[], Some(target)),
            StagingPlan::Unavailable {
                required_bytes: required_staging_bytes(gib(6))
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
                Some(shrink_target(StorageMedia::Rotational, 20, 100))
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
}
