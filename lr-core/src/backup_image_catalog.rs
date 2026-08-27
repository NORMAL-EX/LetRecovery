//! Pure semantic verification for completed WIM/ESD backup catalogs.
//!
//! Byte-level integrity and atomic publication are separate boundaries. This module proves that
//! a fresh capture contains exactly one requested image, or that an append preserved every old
//! image's stable user-visible metadata and added exactly one requested tail image.

use std::fmt;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackupImageMetadata {
    pub name: String,
    pub description: String,
}

impl BackupImageMetadata {
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackupImageCatalog {
    images: Vec<BackupImageMetadata>,
}

impl BackupImageCatalog {
    pub fn new(images: Vec<BackupImageMetadata>) -> Self {
        Self { images }
    }

    pub fn images(&self) -> &[BackupImageMetadata] {
        &self.images
    }

    pub fn len(&self) -> usize {
        self.images.len()
    }

    pub fn is_empty(&self) -> bool {
        self.images.is_empty()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackupCatalogError(&'static str);

impl fmt::Display for BackupCatalogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for BackupCatalogError {}

fn requested_image(name: &str, description: &str) -> BackupImageMetadata {
    BackupImageMetadata::new(name, description)
}

fn checked_appended_count(base_count: usize) -> Result<usize, BackupCatalogError> {
    base_count
        .checked_add(1)
        .ok_or(BackupCatalogError("base image count cannot be incremented"))
}

pub fn verify_fresh_catalog(
    completed: &BackupImageCatalog,
    requested_name: &str,
    requested_description: &str,
) -> Result<(), BackupCatalogError> {
    if completed.images.as_slice()
        != [requested_image(requested_name, requested_description)].as_slice()
    {
        return Err(BackupCatalogError(
            "fresh backup must contain exactly the requested image",
        ));
    }
    Ok(())
}

pub fn verify_append_catalog(
    base: &BackupImageCatalog,
    completed: &BackupImageCatalog,
    requested_name: &str,
    requested_description: &str,
) -> Result<(), BackupCatalogError> {
    if base.is_empty() {
        return Err(BackupCatalogError("append base contains no image"));
    }
    let expected_count = checked_appended_count(base.len())?;
    if completed.len() != expected_count {
        return Err(BackupCatalogError(
            "append must add exactly one image to the base catalog",
        ));
    }
    if completed.images[..base.len()] != base.images {
        return Err(BackupCatalogError(
            "append changed or reordered existing image metadata",
        ));
    }
    if completed.images.last() != Some(&requested_image(requested_name, requested_description)) {
        return Err(BackupCatalogError(
            "appended tail image metadata does not match the request",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(name: &str, description: &str) -> BackupImageMetadata {
        BackupImageMetadata::new(name, description)
    }

    fn catalog(images: Vec<BackupImageMetadata>) -> BackupImageCatalog {
        BackupImageCatalog::new(images)
    }

    #[test]
    fn fresh_requires_exactly_one_requested_image() {
        assert!(verify_fresh_catalog(&catalog(vec![image("new", "desc")]), "new", "desc").is_ok());
        assert!(verify_fresh_catalog(&catalog(vec![]), "new", "desc").is_err());
        assert!(verify_fresh_catalog(
            &catalog(vec![image("new", "desc"), image("extra", "")]),
            "new",
            "desc"
        )
        .is_err());
        assert!(
            verify_fresh_catalog(&catalog(vec![image("new", "wrong")]), "new", "desc").is_err()
        );
    }

    #[test]
    fn append_accepts_unchanged_prefix_and_one_exact_tail() {
        let base = catalog(vec![image("one", "first"), image("two", "second")]);
        let completed = catalog(vec![
            image("one", "first"),
            image("two", "second"),
            image("new", "tail"),
        ]);
        assert!(verify_append_catalog(&base, &completed, "new", "tail").is_ok());
    }

    #[test]
    fn append_rejects_wrong_image_count() {
        let base = catalog(vec![image("old", "base")]);
        assert!(verify_append_catalog(&base, &base, "new", "tail").is_err());
        let two_new = catalog(vec![
            image("old", "base"),
            image("new", "tail"),
            image("extra", "unexpected"),
        ]);
        assert!(verify_append_catalog(&base, &two_new, "new", "tail").is_err());
    }

    #[test]
    fn append_rejects_missing_reordered_or_changed_old_metadata() {
        let base = catalog(vec![image("one", "first"), image("two", "second")]);
        let cases = [
            catalog(vec![image("two", "second"), image("new", "tail")]),
            catalog(vec![
                image("two", "second"),
                image("one", "first"),
                image("new", "tail"),
            ]),
            catalog(vec![
                image("renamed", "first"),
                image("two", "second"),
                image("new", "tail"),
            ]),
            catalog(vec![
                image("one", "changed"),
                image("two", "second"),
                image("new", "tail"),
            ]),
        ];
        for completed in cases {
            assert!(verify_append_catalog(&base, &completed, "new", "tail").is_err());
        }
    }

    #[test]
    fn append_rejects_wrong_tail_metadata_and_empty_base() {
        let base = catalog(vec![image("old", "base")]);
        assert!(verify_append_catalog(
            &base,
            &catalog(vec![image("old", "base"), image("wrong", "tail")]),
            "new",
            "tail"
        )
        .is_err());
        assert!(verify_append_catalog(
            &base,
            &catalog(vec![image("old", "base"), image("new", "wrong")]),
            "new",
            "tail"
        )
        .is_err());
        assert!(verify_append_catalog(
            &catalog(vec![]),
            &catalog(vec![image("new", "tail")]),
            "new",
            "tail"
        )
        .is_err());
    }

    #[test]
    fn append_count_overflow_is_rejected() {
        assert!(checked_appended_count(usize::MAX).is_err());
    }
}
