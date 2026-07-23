//! Pure PE native-window geometry used by the low-resolution and DPI paths.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PixelRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl PixelRect {
    pub(crate) const fn right(self) -> i32 {
        self.x + self.width
    }

    pub(crate) const fn bottom(self) -> i32 {
        self.y + self.height
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ProgressGeometry {
    pub pad: i32,
    pub row_height: i32,
    pub title: PixelRect,
    pub subtitle: Option<PixelRect>,
    pub step_caption: Option<PixelRect>,
    pub step_bar: Option<PixelRect>,
    pub overall_caption: PixelRect,
    pub overall_bar: PixelRect,
    pub rows: PixelRect,
    pub status: PixelRect,
    pub command: PixelRect,
}

pub(crate) fn progress_geometry(
    width: i32,
    height: i32,
    dpi: u32,
    progress_caption_width: i32,
    has_step: bool,
    row_count: usize,
    reserve_command_bar: bool,
) -> ProgressGeometry {
    let width = width.max(1);
    let height = height.max(1);
    let scale = |value| scaled(value, dpi);
    let outer_pad = scale(20).min((width / 16).max(8));
    let content_width = (width - outer_pad * 2).max(1).min(scale(720));
    let pad = (width - content_width) / 2;
    let top = scale(16).min(24);
    let gap = scale(6).min(12);
    let progress_row_height = scale(30);
    let row_height = scale(24);
    let title_height = scale(40);
    let subtitle_height = scale(22);
    let bar_height = scale(10);
    let button_height = scale(30);
    let command_height = if reserve_command_bar {
        (button_height + scale(12).min(24)).min(height / 3).max(1)
    } else {
        0
    };
    let command_top = (height - command_height).max(0);
    let command = PixelRect {
        x: 0,
        y: command_top,
        width,
        height: height - command_top,
    };

    let progress_group_height = progress_row_height;
    let status_minimum = minimum_status_height(dpi).min((command_top - top).max(1));
    let full_step_height = if has_step {
        progress_group_height + gap
    } else {
        0
    };
    let full_mandatory_height =
        title_height + gap + full_step_height + progress_group_height + gap + status_minimum;
    let content_budget = (command_top - top - gap).max(1);
    let compact_step = has_step && content_budget < full_mandatory_height;
    let flow_gap = if compact_step { scale(4).min(8) } else { gap };
    let label_gap = scale(3).min(6);
    // The running page deliberately omits the redundant "do not close" subtitle. Keeping this
    // geometry slot disabled also keeps the live step bar close to the title instead of leaving an
    // unexplained blank row.
    let show_subtitle = false;

    let mut title = PixelRect {
        x: pad,
        y: top,
        width: content_width,
        height: title_height.min((command_top - top).max(1)),
    };
    let mut cursor = title.bottom() + flow_gap;
    let mut subtitle = show_subtitle.then(|| {
        let rect = PixelRect {
            x: pad,
            y: cursor,
            width: content_width,
            height: subtitle_height,
        };
        cursor = rect.bottom() + flow_gap;
        rect
    });

    let caption_width = progress_caption_width
        .max(1)
        .min((content_width / 3).max(1));
    let group = |y: i32| {
        let caption = PixelRect {
            x: pad,
            y,
            width: caption_width,
            height: progress_row_height,
        };
        let bar = PixelRect {
            x: caption.right() + label_gap,
            y: y + (progress_row_height - bar_height).max(0) / 2,
            width: (content_width - caption_width - label_gap).max(1),
            height: bar_height,
        };
        (caption, bar)
    };

    let (mut step_caption, mut step_bar) = if has_step && compact_step {
        (None, None)
    } else if has_step {
        let (caption, bar) = group(cursor);
        cursor = caption.bottom() + flow_gap;
        (Some(caption), Some(bar))
    } else {
        (None, None)
    };
    let (mut overall_caption, mut overall_bar) = group(cursor);
    cursor = overall_caption.bottom();

    let status_bottom = if reserve_command_bar {
        (command_top - flow_gap).max(cursor)
    } else {
        command_top.max(cursor)
    };
    let status = PixelRect {
        x: pad,
        y: status_bottom,
        width: content_width,
        height: 0,
    };
    let row_gap = if row_count == 0 { 0 } else { scale(14) };
    let rows_top = (cursor + row_gap).min(status_bottom);
    let single_column_height = row_height.saturating_mul(row_count as i32);
    let two_column_height = row_height.saturating_mul(row_count.div_ceil(2) as i32);
    let available_rows_height = (status_bottom - rows_top).max(0);
    let (rows_height, all_rows_fit) = if single_column_height <= available_rows_height {
        (single_column_height, true)
    } else if two_column_height <= available_rows_height {
        (two_column_height, true)
    } else {
        (available_rows_height, false)
    };
    let natural_bottom = rows_top + rows_height;
    let content_height = (natural_bottom - top).max(1);
    // Keep the heading and progress summary slightly above the mathematical centre while the
    // complete block still consumes otherwise unused vertical space.
    let centered_top = ((status_bottom - content_height) / 2 - scale(32)).max(top);
    let vertical_shift = if all_rows_fit {
        (centered_top - top).max(0)
    } else {
        0
    };
    let shift = |rect: &mut PixelRect| rect.y += vertical_shift;
    shift(&mut title);
    if let Some(rect) = &mut subtitle {
        shift(rect);
    }
    if let Some(rect) = &mut step_caption {
        shift(rect);
    }
    if let Some(rect) = &mut step_bar {
        shift(rect);
    }
    shift(&mut overall_caption);
    shift(&mut overall_bar);
    let rows = PixelRect {
        x: pad,
        y: rows_top + vertical_shift,
        width: content_width,
        height: rows_height,
    };

    ProgressGeometry {
        pad,
        row_height,
        title,
        subtitle,
        step_caption,
        step_bar,
        overall_caption,
        overall_bar,
        rows,
        status,
        command,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ShellGeometry {
    pub pad: i32,
    pub title: PixelRect,
    pub subtitle: Option<PixelRect>,
    pub body: PixelRect,
    pub command: PixelRect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CommandBarGeometry {
    pub back: Option<PixelRect>,
    pub details: Option<PixelRect>,
    pub close: PixelRect,
    pub footer: Option<PixelRect>,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn command_bar_geometry(
    command: PixelRect,
    pad: i32,
    gap: i32,
    button_height: i32,
    back_width: i32,
    details_width: i32,
    close_width: i32,
    show_back: bool,
    show_details: bool,
) -> CommandBarGeometry {
    let gap = gap.max(0);
    let content_width = (command.width - pad * 2).max(1);
    let visible_count = 1 + usize::from(show_back) + usize::from(show_details);
    let gap_count = visible_count.saturating_sub(1) as i32;
    let maximum = ((content_width - gap * gap_count).max(1) / visible_count as i32).max(1);
    let close_width = close_width.clamp(1, maximum);
    let back_width = back_width.clamp(1, maximum);
    let details_width = details_width.clamp(1, maximum);
    let y = command.y + (command.height - button_height).max(0) / 2;
    let height = button_height.min(command.height).max(1);
    let close = PixelRect {
        x: command.right() - pad - close_width,
        y,
        width: close_width,
        height,
    };
    let details = show_details.then(|| PixelRect {
        x: close.x - gap - details_width,
        y,
        width: details_width,
        height,
    });
    let back = show_back.then(|| PixelRect {
        x: command.x + pad,
        y,
        width: back_width,
        height,
    });
    let footer_left = back.map_or(command.x + pad, |rect| rect.right() + gap);
    let footer_right = details.map_or(close.x - gap, |rect| rect.x - gap);
    let footer = (footer_right > footer_left).then(|| PixelRect {
        x: footer_left,
        y,
        width: footer_right - footer_left,
        height,
    });
    CommandBarGeometry {
        back,
        details,
        close,
        footer,
    }
}

pub(crate) fn shell_geometry(width: i32, height: i32, dpi: u32) -> ShellGeometry {
    let width = width.max(1);
    let height = height.max(1);
    let scale = |value| scaled(value, dpi);
    let pad = scale(20).min((width / 16).max(8));
    let content_width = (width - pad * 2).max(1);
    let top = scale(16).min(24);
    let gap = scale(6).min(12);
    let title_height = scale(28);
    let subtitle_height = scale(24);
    let button_height = scale(30);
    let command_height = (button_height + scale(12).min(24)).min(height / 3).max(1);
    let command_top = (height - command_height).max(0);
    let title = PixelRect {
        x: pad,
        y: top,
        width: content_width,
        height: title_height.min((command_top - top).max(1)),
    };
    let show_subtitle = command_top - title.bottom() >= subtitle_height + gap + scale(72);
    let subtitle = show_subtitle.then(|| PixelRect {
        x: pad,
        y: title.bottom() + gap,
        width: content_width,
        height: subtitle_height,
    });
    let body_top = subtitle.map_or(title.bottom() + gap, |rect| rect.bottom() + gap);
    let body = PixelRect {
        x: pad,
        y: body_top,
        width: content_width,
        height: (command_top - gap - body_top).max(1),
    };
    ShellGeometry {
        pad,
        title,
        subtitle,
        body,
        command: PixelRect {
            x: 0,
            y: command_top,
            width,
            height: height - command_top,
        },
    }
}

pub(crate) fn scaled(value: i32, dpi: u32) -> i32 {
    ((i64::from(value) * i64::from(dpi.max(1)) + 48) / 96) as i32
}

pub(crate) fn minimum_status_height(dpi: u32) -> i32 {
    let font_height = ((10_i64 * i64::from(dpi.max(1)) + 36) / 72) as i32;
    font_height + scaled(8, dpi).min(16)
}

pub(crate) fn clamp_rect_to_work_area(rect: PixelRect, work: PixelRect) -> PixelRect {
    let width = rect.width.max(1).min(work.width.max(1));
    let height = rect.height.max(1).min(work.height.max(1));
    let max_x = work.right() - width;
    let max_y = work.bottom() - height;
    PixelRect {
        x: rect.x.clamp(work.x, max_x.max(work.x)),
        y: rect.y.clamp(work.y, max_y.max(work.y)),
        width,
        height,
    }
}

pub(crate) fn centered_rect_in_work_area(width: i32, height: i32, work: PixelRect) -> PixelRect {
    let width = width.max(1).min(work.width.max(1));
    let height = height.max(1).min(work.height.max(1));
    PixelRect {
        x: work.x + (work.width - width) / 2,
        y: work.y + (work.height - height) / 2,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DPIS: [u32; 5] = [96, 120, 144, 168, 192];
    const SIZES: [(i32, i32); 4] = [
        (640, 440),
        (800, 600),
        // Conservative client areas for a 640x440 outer window. The exact non-client height
        // varies by Windows build and DPI, so both common and worst-case observations are kept.
        (624, 401),
        (624, 378),
    ];

    #[test]
    fn progress_regions_never_overlap_the_command_bar_at_supported_low_sizes() {
        for (width, height) in SIZES {
            for dpi in DPIS {
                for has_step in [false, true] {
                    let row_count = if has_step { 11 } else { 0 };
                    let layout = progress_geometry(
                        width,
                        height,
                        dpi,
                        scaled(52, dpi),
                        has_step,
                        row_count,
                        true,
                    );
                    assert!(layout.title.bottom() <= layout.command.y);
                    assert!(layout.overall_bar.bottom() <= layout.rows.y);
                    assert!(layout.status.bottom() <= layout.command.y);
                    assert_eq!(layout.status.height, 0);
                    assert!(layout.rows.bottom() <= layout.command.y);
                    assert_eq!(layout.step_bar.is_some(), layout.step_caption.is_some());
                    if let Some(subtitle) = layout.subtitle {
                        assert!(subtitle.bottom() <= layout.overall_caption.y);
                    }
                }
            }
        }
    }

    #[test]
    fn roomy_progress_page_centers_the_complete_workflow_block() {
        let dpi = 144;
        let layout = progress_geometry(1180, 846, dpi, scaled(52, dpi), true, 11, false);

        assert_eq!(layout.rows.height, scaled(24, dpi) * 11);
        assert!(layout.title.y > scaled(16, dpi).min(24));
        assert_eq!(layout.command.y, 846);
        assert_eq!(layout.command.height, 0);
        assert!(layout.title.y < 108);
        assert!(layout.rows.bottom() < 846);
    }

    #[test]
    fn compact_default_progress_client_keeps_install_steps_in_one_column() {
        let dpi = 144;
        let layout = progress_geometry(700, 606, dpi, scaled(52, dpi), true, 10, false);

        assert_eq!(layout.rows.height, scaled(24, dpi) * 10);
        assert!(layout.rows.bottom() <= 606);
        let step_caption = layout.step_caption.expect("step caption");
        let step_bar = layout.step_bar.expect("step bar");
        assert_eq!(step_bar.x, layout.overall_bar.x);
        assert_eq!(step_bar.width, layout.overall_bar.width);
        assert_eq!(step_bar.height, scaled(10, dpi));
        assert_eq!(step_caption.x, layout.overall_caption.x);
        assert_eq!(step_caption.width, layout.overall_caption.width);
        assert_eq!(step_bar.x - step_caption.right(), scaled(3, dpi).min(6));
    }

    #[test]
    fn shell_regions_stay_above_the_command_bar_at_supported_low_sizes() {
        for (width, height) in SIZES {
            for dpi in DPIS {
                let layout = shell_geometry(width, height, dpi);
                assert!(layout.title.bottom() <= layout.command.y);
                assert!(layout.body.y >= layout.title.bottom());
                assert!(layout.body.bottom() <= layout.command.y);
                assert!(layout.body.height > 0);
            }
        }
    }

    #[test]
    fn work_area_clamping_preserves_visibility() {
        let work = PixelRect {
            x: 100,
            y: 50,
            width: 800,
            height: 560,
        };
        let clamped = clamp_rect_to_work_area(
            PixelRect {
                x: -200,
                y: 500,
                width: 1400,
                height: 900,
            },
            work,
        );
        assert_eq!(clamped, work);
    }

    #[test]
    fn preferred_window_is_centered_inside_offset_work_area() {
        let work = PixelRect {
            x: 1920,
            y: 40,
            width: 1280,
            height: 680,
        };
        assert_eq!(
            centered_rect_in_work_area(800, 600, work),
            PixelRect {
                x: 2160,
                y: 80,
                width: 800,
                height: 600,
            }
        );
        assert_eq!(centered_rect_in_work_area(1600, 900, work), work);
    }

    #[test]
    fn command_buttons_and_footer_never_overlap_in_the_low_size_dpi_matrix() {
        for (width, height) in SIZES {
            for dpi in DPIS {
                let layout = progress_geometry(width, height, dpi, scaled(52, dpi), true, 11, true);
                let command = command_bar_geometry(
                    layout.command,
                    layout.pad,
                    scaled(8, dpi),
                    scaled(30, dpi),
                    scaled(240, dpi),
                    scaled(320, dpi),
                    scaled(180, dpi),
                    true,
                    true,
                );
                let back = command.back.unwrap();
                let details = command.details.unwrap();
                assert!(back.right() <= details.x);
                assert!(details.right() <= command.close.x);
                assert!(command.close.right() <= layout.command.right() - layout.pad);
                if let Some(footer) = command.footer {
                    assert!(back.right() <= footer.x);
                    assert!(footer.right() <= details.x);
                }
            }
        }
    }
}
