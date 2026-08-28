use std::collections::HashMap;

use galaxyui::elements::{
    AfterLayoutContext, AppContext, Element, EventContext, LayoutContext, PaintContext, Point,
    SizeConstraint,
};
use galaxyui::event::DispatchedEvent;
use galaxyui::fonts::Properties;
use galaxyui::geometry::rect::RectF;
use galaxyui::units::IntoPixels;
use galaxyui::EntityId;
use pathfinder_geometry::vector::{vec2f, Vector2F};

use super::blockgrid_renderer::GridRenderParams;
use crate::appearance::Appearance;
use crate::settings::EnforceMinimumContrast;
use crate::terminal::blockgrid_renderer::BlockGridParams;
use crate::terminal::grid_renderer::{self, CellGlyphCache};
use crate::terminal::grid_size_util::grid_cell_dimensions;
use crate::terminal::model::blockgrid::BlockGrid;
use crate::terminal::model::grid::grid_handler::{GridHandler, TermMode};
use crate::terminal::model::grid::{Dimensions, RespectDisplayedOutput};
use crate::terminal::model::image_map::StoredImageMetadata;
use crate::terminal::model::ObfuscateSecrets;
use crate::terminal::{color, SizeInfo};

pub struct BlockGridElement {
    block_grid: BlockGrid,
    block_grid_params: BlockGridParams,
    size: Vector2F,
    origin: Option<Point>,
    bounds: Option<RectF>,
}

impl BlockGridElement {
    pub fn new(
        block_grid: &BlockGrid,
        appearance: &Appearance,
        enforce_minimum_contrast: EnforceMinimumContrast,
        obfuscate_secrets: ObfuscateSecrets,
        size_info: SizeInfo,
    ) -> Self {
        let theme = appearance.theme();
        let cell_size = Vector2F::new(
            size_info.cell_width_px().as_f32(),
            size_info.cell_height_px().as_f32(),
        );
        let size = vec2f(
            block_grid.grid_handler().columns() as f32,
            block_grid.len_displayed() as f32,
        ) * cell_size;
        Self {
            block_grid: block_grid.clone(),
            block_grid_params: BlockGridParams {
                grid_render_params: GridRenderParams {
                    warp_theme: theme.clone(),
                    font_family: appearance.monospace_font_family(),
                    font_size: appearance.monospace_font_size(),
                    font_weight: appearance.monospace_font_weight(),
                    line_height_ratio: appearance.ui_builder().line_height_ratio(),
                    enforce_minimum_contrast,
                    obfuscate_secrets,
                    size_info,
                    cell_size,
                    use_ligature_rendering: false,
                    hide_cursor_cell: false,
                },
                colors: color::List::from(&color::Colors::from(theme.clone())),
                override_colors: color::OverrideList::empty(),
                bounds: Default::default(),
            },
            size,
            origin: None,
            bounds: None,
        }
    }

    pub fn with_ligature_rendering(mut self) -> Self {
        self.block_grid_params
            .grid_render_params
            .use_ligature_rendering = true;
        self
    }

    /// Returns the underlying text of the `BlockGrid`.
    pub fn text(&self) -> String {
        self.block_grid.contents_to_string(false, None)
    }
}

impl Element for BlockGridElement {
    fn layout(
        &mut self,
        constraint: SizeConstraint,
        _ctx: &mut LayoutContext,
        _app: &AppContext,
    ) -> Vector2F {
        self.size = self.size.min(constraint.max);
        self.size
    }

    fn after_layout(&mut self, _ctx: &mut AfterLayoutContext, _app: &AppContext) {}

    fn paint(&mut self, origin: Vector2F, ctx: &mut PaintContext, app: &AppContext) {
        self.origin = Some(Point::from_vec2f(origin, ctx.scene.z_index()));
        let bounds = RectF::new(origin, self.size);

        self.block_grid_params.bounds = bounds;
        self.bounds = Some(bounds);
        self.block_grid
            .draw_with_default_params(origin, origin, &self.block_grid_params, ctx, app);
    }

    fn dispatch_event(
        &mut self,
        _event: &DispatchedEvent,
        _ctx: &mut EventContext,
        _app: &AppContext,
    ) -> bool {
        false
    }

    fn size(&self) -> Option<Vector2F> {
        Some(self.size)
    }

    fn origin(&self) -> Option<Point> {
        self.origin
    }
}

/// A paint-only snapshot of a live terminal grid.
///
/// Unlike [`BlockGridElement`], this can render both normal command output and an alternate-screen
/// grid. It deliberately owns a snapshot so the terminal model lock never spans layout or paint.
pub struct TerminalGridSnapshotElement {
    grid: GridHandler,
    row_count: usize,
    colors: color::List,
    override_colors: color::OverrideList,
    image_metadata: HashMap<u32, StoredImageMetadata>,
    grid_render_params: GridRenderParams,
    terminal_view_id: EntityId,
    natural_size: Vector2F,
    size: Vector2F,
    origin: Option<Point>,
    bounds: Option<RectF>,
}

impl TerminalGridSnapshotElement {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        grid: &GridHandler,
        row_count: usize,
        colors: color::List,
        override_colors: color::OverrideList,
        image_metadata: HashMap<u32, StoredImageMetadata>,
        appearance: &Appearance,
        enforce_minimum_contrast: EnforceMinimumContrast,
        obfuscate_secrets: ObfuscateSecrets,
        terminal_view_id: EntityId,
        app: &AppContext,
    ) -> Self {
        let cell_size = grid_cell_dimensions(
            app.font_cache(),
            appearance.monospace_font_family(),
            appearance.monospace_font_size(),
            appearance.line_height_ratio(),
        );
        let size = vec2f(
            grid.columns() as f32 * cell_size.x(),
            row_count as f32 * cell_size.y(),
        );
        let size_info = SizeInfo::new(
            size,
            cell_size.x().into_pixels(),
            cell_size.y().into_pixels(),
            0.0.into_pixels(),
            0.0.into_pixels(),
        )
        .with_rows_and_columns(row_count, grid.columns());

        Self {
            grid: grid.clone(),
            row_count,
            colors,
            override_colors,
            image_metadata,
            grid_render_params: GridRenderParams {
                warp_theme: appearance.theme().clone(),
                font_family: appearance.monospace_font_family(),
                font_size: appearance.monospace_font_size(),
                font_weight: appearance.monospace_font_weight(),
                line_height_ratio: appearance.line_height_ratio(),
                enforce_minimum_contrast,
                obfuscate_secrets,
                size_info,
                cell_size,
                use_ligature_rendering: false,
                hide_cursor_cell: false,
            },
            terminal_view_id,
            natural_size: size,
            size,
            origin: None,
            bounds: None,
        }
    }

    pub fn with_ligature_rendering(mut self) -> Self {
        self.grid_render_params.use_ligature_rendering = true;
        self
    }
}

impl Element for TerminalGridSnapshotElement {
    fn layout(
        &mut self,
        constraint: SizeConstraint,
        _ctx: &mut LayoutContext,
        _app: &AppContext,
    ) -> Vector2F {
        // The parent clips and scrolls this element vertically, so preserve the full terminal
        // height here. Clamping it to the viewport makes the scroll container believe rows below
        // the fold do not exist, which also prevents its bottom-follow behavior from engaging.
        self.size = vec2f(
            self.natural_size
                .x()
                .min(constraint.max.x())
                .max(constraint.min.x()),
            self.natural_size.y().max(constraint.min.y()),
        );
        self.size
    }

    fn after_layout(&mut self, _ctx: &mut AfterLayoutContext, _app: &AppContext) {}

    fn paint(&mut self, origin: Vector2F, ctx: &mut PaintContext, app: &AppContext) {
        self.origin = Some(Point::from_vec2f(origin, ctx.scene.z_index()));
        self.bounds = Some(RectF::new(origin, self.size));

        let Some(visible_bounds) = ctx.scene.visible_rect(
            self.origin.expect("origin was set immediately above"),
            self.size,
        ) else {
            return;
        };
        let row_height = self.grid_render_params.cell_size.y();
        let start_row = ((visible_bounds.min_y() - origin.y()) / row_height)
            .floor()
            .max(0.) as usize;
        let end_row = ((visible_bounds.max_y() - origin.y()) / row_height)
            .ceil()
            .max(0.) as usize;
        let start_row = start_row.min(self.row_count);
        let end_row = end_row.min(self.row_count);

        let mut glyphs = CellGlyphCache::default();
        let cursor_visible = self.grid.is_mode_set(TermMode::SHOW_CURSOR)
            && (start_row..end_row).contains(&self.grid.cursor_render_point().row);
        let cursor_style = self.grid.cursor_style();
        let obfuscate_secrets = self
            .grid_render_params
            .obfuscate_secrets
            .and(&self.grid.get_secret_obfuscation());

        grid_renderer::render_grid(
            &self.grid,
            start_row,
            end_row,
            &self.colors,
            &self.override_colors,
            &self.grid_render_params.warp_theme,
            Properties::default().weight(self.grid_render_params.font_weight),
            self.grid_render_params.font_family,
            self.grid_render_params.font_size,
            self.grid_render_params.line_height_ratio,
            self.grid_render_params.cell_size,
            self.grid_render_params.size_info.padding_x_px(),
            origin,
            &mut glyphs,
            255,
            None,
            None,
            None::<std::iter::Empty<&std::ops::RangeInclusive<crate::terminal::model::index::Point>>>,
            None,
            self.grid_render_params.enforce_minimum_contrast,
            obfuscate_secrets,
            None,
            self.grid_render_params.use_ligature_rendering,
            cursor_visible.then_some(cursor_style.shape),
            RespectDisplayedOutput::Yes,
            &self.image_metadata,
            None,
            false,
            ctx,
            app,
        );

        if cursor_visible {
            grid_renderer::render_cursor(
                &self.grid_render_params,
                self.grid.cursor_render_point(),
                self.grid.is_cursor_on_wide_char(),
                cursor_style,
                self.grid_render_params.size_info.padding_x_px(),
                origin,
                self.grid_render_params.warp_theme.cursor().into(),
                ctx,
                self.terminal_view_id,
                None,
                app,
            );
        }
    }

    fn dispatch_event(
        &mut self,
        _event: &DispatchedEvent,
        _ctx: &mut EventContext,
        _app: &AppContext,
    ) -> bool {
        false
    }

    fn size(&self) -> Option<Vector2F> {
        Some(self.size)
    }

    fn origin(&self) -> Option<Point> {
        self.origin
    }
}
