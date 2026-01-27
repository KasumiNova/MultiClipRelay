use gtk4::prelude::*;
use gtk4::{gio, glib};
use gtk4::pango::EllipsizeMode;

#[derive(Clone, Copy)]
pub struct ColumnSpec {
    pub title: &'static str,
    pub fixed_width: Option<i32>,
    pub expand: bool,
    pub resizable: bool,
    pub ellipsize: bool,
}

/// A simple table built on ColumnView.
///
/// Each row is stored as a single tab-separated line in `StringObject`:
///   col0\tcol1\tcol2...
///
/// Rendering splits the line by `\t` and picks the requested column.
#[derive(Clone)]
pub struct TabbedTable {
    pub store: gio::ListStore,
    #[allow(dead_code)]
    pub selection: gtk4::SingleSelection,
    #[allow(dead_code)]
    pub view: gtk4::ColumnView,
    pub scroll: gtk4::ScrolledWindow,
}

fn parse_cols(line: &str, want: usize) -> String {
    // Fast path: walk splitn up to wanted col.
    let mut it = line.split('\t');
    for i in 0..=want {
        let v = it.next().unwrap_or("");
        if i == want {
            return v.to_string();
        }
    }
    String::new()
}

pub fn make_tabbed_table(columns: &[ColumnSpec]) -> TabbedTable {
    let store = gio::ListStore::new::<gtk4::StringObject>();
    let selection = gtk4::SingleSelection::new(Some(store.clone()));
    selection.set_autoselect(false);
    selection.set_can_unselect(true);

    let view = gtk4::ColumnView::new(Some(selection.clone()));
    view.set_vexpand(true);
    view.set_hexpand(true);

    for (idx, c) in columns.iter().enumerate() {
        let factory = gtk4::SignalListItemFactory::new();
        let ellipsize = c.ellipsize;
        factory.connect_setup(move |_, list_item| {
            let label = gtk4::Label::builder()
                .xalign(0.0)
                .selectable(true)
                .single_line_mode(true)
                .ellipsize(if ellipsize {
                    EllipsizeMode::End
                } else {
                    EllipsizeMode::None
                })
                .build();
            label.add_css_class("monospace");
            list_item.set_child(Some(&label));
        });

        factory.connect_bind(move |_, list_item| {
            let Some(item) = list_item.item() else { return; };
            let Ok(obj) = item.downcast::<gtk4::StringObject>() else { return; };
            let text = parse_cols(obj.string().as_str(), idx);
            let Some(child) = list_item.child() else { return; };
            let Ok(label) = child.downcast::<gtk4::Label>() else { return; };
            label.set_text(&text);
        });

        let col = gtk4::ColumnViewColumn::new(Some(c.title), Some(factory));
        if let Some(w) = c.fixed_width {
            col.set_fixed_width(w);
        }
        col.set_expand(c.expand);
        col.set_resizable(c.resizable);
        view.append_column(&col);
    }

    let scroll = gtk4::ScrolledWindow::builder()
        .vexpand(true)
        .hexpand(true)
        .hscrollbar_policy(gtk4::PolicyType::Automatic)
        .vscrollbar_policy(gtk4::PolicyType::Automatic)
        .child(&view)
        .build();

    // Nice-to-have: make rows a bit less cramped.
    view.add_css_class("boxed-list");

    TabbedTable {
        store,
        selection,
        view,
        scroll,
    }
}

/// Utility to keep scroll pinned to bottom when the user is already at the bottom.
///
/// Call this right after you've appended items to the underlying model.
pub fn keep_scroll_tail(scroll: &gtk4::ScrolledWindow, was_at_bottom: bool, old_value: f64, old_upper: f64) {
    glib::idle_add_local_once(glib::clone!(@weak scroll => move || {
        let vadj = scroll.vadjustment();
        if was_at_bottom {
            let upper = vadj.upper();
            let page = vadj.page_size();
            let max_value = (upper - page).max(0.0);
            vadj.set_value(max_value);
        } else {
            // Best-effort: keep relative position.
            let upper = vadj.upper();
            let page = vadj.page_size();
            let max_value = (upper - page).max(0.0);
            if old_upper > 1.0 && upper > 1.0 {
                let frac = (old_value / old_upper).clamp(0.0, 1.0);
                vadj.set_value((frac * upper).min(max_value));
            } else {
                vadj.set_value(old_value.min(max_value));
            }
        }
    }));
}
