# material

Material Design 3, as a Kite package.

A design system is not part of a language's standard library. [`std/ui`](../../std/ui.kite)
is a layout engine and a painting model with no opinion about how a button
looks; this is one answer to that question, and an iOS or Fluent package would
be a different answer against the same core.

```kite
use material

fn view(model: Model) -> ui.Node<Msg> {
    let s = material.dark()
    return material.filled_button(
        s,
        "save",
        Msg.Save,
        "Save",
        material.state_of(model.pointer, "save"),
    )
}
```

A component is a **function that returns a node**. There is no component object
and no internal state: every one takes the `Scheme` rather than reading a
global, and every interactive one takes an `id` and a message from the caller
rather than inventing either.

## The components

Every component on <https://m3.material.io/components>, and where it lives.

| Material | Here |
| --- | --- |
| [Badges](https://m3.material.io/components/badges) | `badge`, `small_badge` |
| [Bottom app bar](https://m3.material.io/components/bottom-app-bar) | `bottom_app_bar` |
| [Bottom sheets](https://m3.material.io/components/bottom-sheets) | `bottom_sheet`, with `scrim` and `modal` for the modal one |
| [Buttons](https://m3.material.io/components/buttons) | `filled_button`, `tonal_button`, `outlined_button`, `text_button`, `elevated_button`, `icon_label_button` — each with an `_of` taking `button_xsmall` … `button_xlarge` |
| [Button groups](https://m3.material.io/components/button-groups) | `button_group`, `connected_button_group`, `squeezed_widths` |
| [Cards](https://m3.material.io/components/cards) | `elevated_card`, `filled_card`, `outlined_card`, `clickable_card` |
| [Carousel](https://m3.material.io/components/carousel) | `hero`, `multi_browse`, `uncontained`, `full_screen`; `carousel`, `carousel_item`, `clickable_carousel_item`, `carousel_caption`, `item_widths`, `carousel_extent` |
| [Checkbox](https://m3.material.io/components/checkbox) | `checkbox`, `checkbox_mixed` |
| [Chips](https://m3.material.io/components/chips) | `assist_chip`, `filter_chip`, `input_chip`, `suggestion_chip` |
| [Date pickers](https://m3.material.io/components/date-pickers) | `docked_date_picker`, `modal_date_picker`, `date_input`, `date_range_picker`; `calendar`, `month_nav`, `day_id`, and the calendar arithmetic below |
| [Dialogs](https://m3.material.io/components/dialogs) | `dialog`, `full_screen_dialog`, `modal`, `scrim` |
| [Divider](https://m3.material.io/components/divider) | `divider`, `inset_divider` |
| [Extended FAB](https://m3.material.io/components/extended-fab) | `extended_fab` |
| [FAB](https://m3.material.io/components/floating-action-button) | `fab`, `small_fab`, `large_fab`, `fab_of`, `secondary_fab`, `tertiary_fab` |
| [FAB menu](https://m3.material.io/components/fab-menu) | `fab_menu`, `fab_menu_item`, `fab_menu_toggle`, `stagger` |
| [Icon buttons](https://m3.material.io/components/icon-buttons) | `icon_button`, `filled_icon_button`, `tonal_icon_button`, `outlined_icon_button`, and a toggle for each: `icon_toggle`, `filled_icon_toggle`, `tonal_icon_toggle`, `outlined_icon_toggle` — all with an `_of` taking `icon_button_xsmall` … `icon_button_xlarge` |
| [Lists](https://m3.material.io/components/lists) | `list_item`, `list_item_two`, `list_item_three`, `list_item_with`, `list_subheader` |
| [Loading indicator](https://m3.material.io/components/loading-indicator) | `loading_indicator` with `paint_loading` or `paint_contained_loading` |
| [Menus](https://m3.material.io/components/menus) | `menu`, `menu_item`, `select` |
| [Navigation bar](https://m3.material.io/components/navigation-bar) | `navigation_bar`, `bar_item` |
| [Navigation drawer](https://m3.material.io/components/navigation-drawer) | `navigation_drawer`, `drawer_item`, `drawer_headline` |
| [Navigation rail](https://m3.material.io/components/navigation-rail) | `navigation_rail`, `rail_item`; `navigation_rail_expanded`, `expanded_rail_item` |
| [Progress indicators](https://m3.material.io/components/progress-indicators) | `linear_progress`, `linear_indeterminate`; `circular_progress` with `paint_circular` or `paint_circular_indeterminate`; `phase_of` |
| [Radio button](https://m3.material.io/components/radio-button) | `radio` |
| [Search](https://m3.material.io/components/search) | `search_bar`, `search_field`, `search_view`, `docked_search_view` |
| [Segmented buttons](https://m3.material.io/components/segmented-buttons) | `segmented_button`, `segment` |
| [Side sheets](https://m3.material.io/components/side-sheets) | `side_sheet` |
| [Sliders](https://m3.material.io/components/sliders) | `slider`, `slider_of` (discrete), `range_slider`, `centred_slider`, `vertical_slider`; `slider_value_at`, `range_low_at`, `range_high_at`, `grabbing_low`, `centred_value_at`, `vertical_value_at` |
| [Snackbar](https://m3.material.io/components/snackbar) | `snackbar`, `snackbar_action` |
| [Split button](https://m3.material.io/components/split-button) | `split_button`, `split_action` |
| [Switch](https://m3.material.io/components/switch) | `switch` |
| [Tabs](https://m3.material.io/components/tabs) | `tab`, `secondary_tab`, `icon_tab`, `tab_row` |
| [Text fields](https://m3.material.io/components/text-fields) | `filled_field`, `outlined_field`, `text_area`, `select`; `field` and the `_of` variants for a label, supporting text and an error at once |
| [Time pickers](https://m3.material.io/components/time-pickers) | `time_picker`, `time_input`; `time_dial` with `paint_time_dial` and `dial_value_at`; `time_numeral`, `period_toggle`, `clock` |
| [Toolbars](https://m3.material.io/components/toolbars) | `docked_toolbar`, `floating_toolbar`, `vibrant_floating_toolbar`, `vertical_floating_toolbar`, `vibrant_vertical_toolbar` |
| [Tooltips](https://m3.material.io/components/tooltips) | `tooltip`, `rich_tooltip` |

## Under them

| | |
| --- | --- |
| [`tokens.kite`](tokens.kite) | The colour roles, the type scale, shape, spacing, elevation, state-layer opacities and the window size classes |
| [`color.kite`](color.kite) | HCT and the tonal palettes: `scheme_from(seed, dark)` generates a whole scheme, and `contrast_between` checks it |
| [`motion.kite`](motion.kite) | Material's easing curves, duration tokens, `Track` and springs |
| [`interaction.kite`](interaction.kite) | One value threaded through `update` that carries every control's hover, focus, press, selection and ripple between frames |
| [`pickers.kite`](pickers.kite) | The calendar: `is_leap_year`, `days_in_month`, `weekday_of`, `ordinal`, `month_cells`, `month_before`, `month_after`, `within` |

## What is not drawn, and why

* **Shadows.** A shadow is a soft alpha gradient and the drawing boundary has
  no blur. Elevation here is *tonal* — Material's own rule for dark themes —
  and continuous, so a card that rises under the pointer changes tone as it
  rises rather than stepping.
* **Icons.** Material Symbols is a font, and a font is not something a package
  can carry. Every component that wants a glyph takes it as a `str`.
* **Per-corner radii.** `Decor` has one radius for all four corners, so a
  connected group's ends are small-cornered rather than half-round, and an
  outlined field's label sits inside the box rather than floating on the
  outline.
* **Arcs, clock faces and morphing shapes.** These are polar, and rows and
  columns are not. Each comes in two halves: a node that reserves the space and
  a painter called after `ui.paint` that fills it — `circular_progress` with
  `paint_circular`, `time_dial` with `paint_time_dial`, `loading_indicator`
  with `paint_loading`. Two calls rather than one is the honest cost, and it is
  written out rather than hidden behind a component that would silently draw
  nothing.

## Tests

[`tests/packages/material_test.kite`](../../tests/packages/material_test.kite)
is an ordinary Kite program, run on the bytecode VM *and* on WebAssembly and
compared — a design system whose easing curve gave different numbers on two
backends would animate differently on two backends. It checks the easing curves
against an independent solve, the colour space against Material's own published
values, the calendar against the civil calendar, and every component for laying
out to a real rectangle with a label on it.

```bash
cargo test --test stdlib
```
