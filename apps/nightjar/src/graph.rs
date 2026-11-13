use crate::{clip, rows, safe_text, text};
use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, Vec2};
use serde_json::Value;

pub const SURFACE: Color32 = Color32::from_rgb(0x1a, 0x1a, 0x19);
pub const INK: Color32 = Color32::from_rgb(0xf0, 0xef, 0xec);
pub const SECONDARY: Color32 = Color32::from_rgb(0xc3, 0xc2, 0xb7);
pub const BLUE: Color32 = Color32::from_rgb(0x39, 0x87, 0xe5);

#[derive(Default)]
pub struct GraphView {
    pub table: bool,
    pub edges: bool,
    pub search: String,
}

pub enum GraphAction {
    Focus(String),
    Inspect(Value),
}

/// A bounded one-hop view; no inference, force simulation or file I/O.
impl GraphView {
    pub fn show(&mut self, ui: &mut egui::Ui, graph: &Value) -> Option<GraphAction> {
        let mut action = None;
        ui.horizontal_wrapped(|ui| {
            ui.heading("Relationships");
            ui.separator();
            ui.selectable_value(&mut self.table, false, "Graph");
            ui.selectable_value(&mut self.table, true, "Table");
        });
        ui.label(egui::RichText::new("Source observations · not identity conclusions").small().color(SECONDARY)).on_hover_text("Only source-attributed collector observations create these directed relationships. Findings and analyst assertions stay separate.");
        let nodes = rows(graph, "nodes");
        let edges = rows(graph, "edges");
        if nodes.is_empty() {
            ui.add_space(16.0);
            ui.heading("No source relationships yet");
            ui.label("Open a collected case or run an explicitly authorized investigation.");
            ui.small("Saving a question does not collect sources.");
            return None;
        }
        ui.horizontal(|ui| {
            ui.label("Find node");
            ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .desired_width(200.0)
                    .hint_text("Exact value or type"),
            );
        });
        if !self.search.trim().is_empty() {
            let query = self.search.to_lowercase();
            let matches: Vec<_> = nodes
                .iter()
                .filter(|n| text(n, "label").to_lowercase().contains(&query))
                .collect();
            egui::ScrollArea::vertical()
                .id_salt("graph-search")
                .max_height(120.0)
                .show(ui, |ui| {
                    for node in matches.iter().take(20) {
                        if ui
                            .button(clip(&node_label(node).0, 54))
                            .on_hover_text(safe_text(text(node, "label")))
                            .clicked()
                        {
                            action = Some(GraphAction::Focus(text(node, "id").into()));
                        }
                    }
                    ui.label(format!(
                        "{} matches · first 20 shown; refine search or use Table",
                        matches.len()
                    ));
                });
        }
        if ui.available_width() < 480.0 {
            self.table = true;
            ui.small("Narrow view · table shown")
                .on_hover_text("Close Details or widen the workspace to return to the graph.");
        }
        if self.table {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.edges, false, format!("Nodes ({})", nodes.len()));
                ui.selectable_value(&mut self.edges, true, format!("Edges ({})", edges.len()));
            });
            ui.label(if self.edges {
                "From → relationship → to"
            } else {
                "Type · original value"
            });
            let records = if self.edges { edges } else { nodes };
            egui::ScrollArea::vertical()
                .id_salt("source-table")
                .max_height(340.0)
                .show_rows(ui, 46.0, records.len(), |ui, range| {
                    for index in range {
                        let record = &records[index];
                        let label = if self.edges {
                            let endpoint = |key| {
                                nodes
                                    .iter()
                                    .find(|n| n["id"] == record[key])
                                    .map(|n| text(n, "label"))
                                    .unwrap_or("(missing)")
                            };
                            format!(
                                "{} → {} → {}",
                                clip(endpoint("source_id"), 30),
                                clip(text(record, "relation"), 28),
                                clip(endpoint("target_id"), 30)
                            )
                        } else {
                            format!(
                                "{} · {}",
                                text(record, "type"),
                                record["value"]
                                    .as_str()
                                    .map(str::to_owned)
                                    .unwrap_or_else(|| record["value"].to_string())
                            )
                        };
                        let response = ui.add_sized(
                            [ui.available_width(), 40.0],
                            egui::Button::new(clip(&label, 110)).wrap(),
                        );
                        if response.on_hover_text(safe_text(&label)).clicked() {
                            action = Some(GraphAction::Inspect(record.clone()));
                        }
                    }
                });
        } else {
            action = self.diagram(ui, graph).or(action);
        }
        ui.collapsing("Coverage & interpretation", |ui| {
            for limitation in rows(graph, "limitations") {
                ui.label(safe_text(limitation.as_str().unwrap_or_default()));
            }
        });
        action
    }

    fn diagram(&self, ui: &mut egui::Ui, graph: &Value) -> Option<GraphAction> {
        let nodes = rows(graph, "nodes");
        let focus = nodes
            .iter()
            .find(|node| node["id"] == graph["focus_id"])
            .unwrap_or(&nodes[0]);
        let incident: Vec<_> = rows(graph, "edges")
            .iter()
            .filter(|edge| edge["source_id"] == focus["id"] || edge["target_id"] == focus["id"])
            .collect();
        ui.horizontal_wrapped(|ui| {
            ui.small(format!("{} nodes · {} relationships", nodes.len(), rows(graph, "edges").len()));
            if incident.len() > 4 { ui.small(format!("4 of {} nearby shown · all in Table", incident.len())); }
            ui.label(egui::RichText::new("? ").small()).on_hover_text("FOCUS + blue outline marks the selected source. Arrows show recorded direction. Select a neighbor to focus or a relationship to inspect provenance.");
        });
        let (rect, _) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 344.0), Sense::hover());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 8.0, SURFACE);
        let center = Pos2::new(rect.left() + 82.0, rect.center().y);
        let neighbor_x = rect.right() - 82.0;
        let mut action = None;
        for (index, edge) in incident.iter().take(4).enumerate() {
            let target_id = if edge["source_id"] == focus["id"] {
                &edge["target_id"]
            } else {
                &edge["source_id"]
            };
            let Some(neighbor) = nodes.iter().find(|n| &n["id"] == target_id) else {
                continue;
            };
            let y = rect.center().y
                + (index as f32 - (incident.len().min(4) as f32 - 1.0) / 2.0) * 78.0;
            let right = Pos2::new(neighbor_x, y);
            let a = Pos2::new(center.x + 72.0, center.y);
            let b = Pos2::new(right.x - 72.0, right.y);
            let (from, to) = if edge["source_id"] == focus["id"] {
                (a, b)
            } else {
                (b, a)
            };
            painter.line_segment([from, to], Stroke::new(2.0_f32, SECONDARY));
            let direction = (to - from).normalized();
            let normal = Vec2::new(-direction.y, direction.x);
            painter.add(egui::Shape::convex_polygon(
                vec![
                    to,
                    to - direction * 10.0 + normal * 5.0,
                    to - direction * 10.0 - normal * 5.0,
                ],
                SECONDARY,
                Stroke::NONE,
            ));
            let mid = a.lerp(b, 0.5);
            let label_rect = Rect::from_center_size(
                mid,
                Vec2::new((rect.width() - 316.0).clamp(75.0, 190.0), 28.0),
            );
            painter.rect_filled(label_rect, 4.0, SURFACE);
            draw_label(
                &painter,
                label_rect.shrink(4.0),
                text(edge, "relation"),
                12.0,
                INK,
            );
            let response = ui
                .interact(
                    label_rect,
                    ui.id().with(("relation", index)),
                    Sense::click(),
                )
                .on_hover_text(format!(
                    "{} → {} → {}\nSource observation; click for exact provenance",
                    safe_text(text(edge, "source_id")),
                    safe_text(text(edge, "relation")),
                    safe_text(text(edge, "target_id"))
                ));
            response.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::Button,
                    true,
                    format!(
                        "Source relation {} from {} to {}",
                        safe_text(text(edge, "relation")),
                        safe_text(text(edge, "source_id")),
                        safe_text(text(edge, "target_id"))
                    ),
                )
            });
            if response.clicked() {
                action = Some(GraphAction::Inspect((*edge).clone()));
            }
            if draw_node(ui, &painter, right, neighbor, false, index).clicked() {
                action = Some(GraphAction::Focus(text(neighbor, "id").into()));
            }
        }
        if draw_node(ui, &painter, center, focus, true, 99).clicked() {
            action = Some(GraphAction::Inspect((*focus).clone()));
        }
        if incident.is_empty() {
            painter.text(
                Pos2::new(neighbor_x, rect.center().y),
                egui::Align2::CENTER_CENTER,
                "No incident source edges",
                egui::FontId::proportional(12.0),
                SECONDARY,
            );
        }
        action
    }
}

/// Display-only abbreviations; exact typed IDs/values and support never change.
fn node_label(node: &Value) -> (String, String) {
    let value = node["value"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| node["value"].to_string());
    if let Some(rest) = value
        .strip_prefix("https://")
        .or_else(|| value.strip_prefix("http://"))
    {
        let clean = rest.split(['?', '#']).next().unwrap_or(rest);
        let (host, path) = clean.split_once('/').unwrap_or((clean, ""));
        let segments: Vec<_> = path.split('/').filter(|s| !s.is_empty()).collect();
        let tail = segments[segments.len().saturating_sub(2)..].join("/");
        return if tail.is_empty() {
            (
                host.trim_start_matches("www.").into(),
                text(node, "type").replace('_', " "),
            )
        } else {
            (tail, host.trim_start_matches("www.").into())
        };
    }
    let (chain, hash) = value.split_once(':').unwrap_or(("", &value));
    if hash.starts_with("0x") && hash.len() > 24 && hash[2..].bytes().all(|b| b.is_ascii_hexdigit())
    {
        return (
            format!("{}…{}", &hash[..10], &hash[hash.len() - 8..]),
            chain.into(),
        );
    }
    (value, String::new())
}

fn draw_label(painter: &egui::Painter, rect: Rect, value: &str, size: f32, color: Color32) {
    let mut job = egui::text::LayoutJob::simple_singleline(
        clip(value, 512),
        egui::FontId::proportional(size),
        color,
    );
    job.wrap.max_width = rect.width();
    job.wrap.max_rows = 1;
    let painter = painter.with_clip_rect(rect);
    let galley = painter.layout_job(job);
    painter.galley(rect.center() - galley.size() / 2.0, galley, color);
}

fn draw_node(
    ui: &mut egui::Ui,
    painter: &egui::Painter,
    position: Pos2,
    node: &Value,
    focus: bool,
    index: usize,
) -> egui::Response {
    let rect = Rect::from_center_size(position, Vec2::new(144.0, 66.0));
    painter.rect_filled(rect, 6.0, Color32::from_rgb(0x24, 0x24, 0x23));
    painter.rect_stroke(
        rect,
        6.0,
        Stroke::new(
            if focus { 2.0_f32 } else { 1.0_f32 },
            if focus { BLUE } else { SECONDARY },
        ),
        egui::StrokeKind::Inside,
    );
    let node_text = |center: Pos2, value: &str, size: f32, color: Color32| {
        draw_label(
            painter,
            Rect::from_center_size(center, Vec2::new(rect.width() - 12.0, 18.0)),
            value,
            size,
            color,
        );
    };
    node_text(
        position - Vec2::new(0.0, 21.0),
        &format!(
            "{}{}",
            if focus { "FOCUS · " } else { "" },
            text(node, "type")
        ),
        11.0,
        SECONDARY,
    );
    let value = node["value"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| node["value"].to_string());
    let (label, context) = node_label(node);
    node_text(position, &label, 13.0, INK);
    if !context.is_empty() {
        node_text(position + Vec2::new(0.0, 20.0), &context, 10.0, SECONDARY);
    }
    let response = ui
        .interact(rect, ui.id().with(("source-node", index)), Sense::click())
        .on_hover_text(format!(
            "[{}] {}\n{}\n{}",
            safe_text(text(node, "type")),
            safe_text(&value),
            safe_text(text(node, "id")),
            if focus {
                "Click to inspect"
            } else {
                "Click to make this the shared focus"
            }
        ));
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            true,
            format!(
                "{} source node [{}] {}",
                if focus { "Focused" } else { "Neighbor" },
                safe_text(text(node, "type")),
                safe_text(&value)
            ),
        )
    });
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn url_labels_distinguish_people_and_paths_without_changing_originals() {
        let a = json!({"type":"profile_url", "value":"https://github.com/alice?tab=repositories"});
        let b = json!({"type":"profile_url", "value":"https://github.com/bob"});
        assert_eq!(node_label(&a), ("alice".into(), "github.com".into()));
        assert_eq!(node_label(&b).0, "bob");
        assert_eq!(
            node_label(
                &json!({"type":"url","value":"https://docs.example.test/long/section/record"})
            )
            .0,
            "section/record"
        );
        assert_eq!(a["value"], "https://github.com/alice?tab=repositories");
    }
    #[test]
    fn node_text_fits_measured_width_not_character_count() {
        let context = egui::Context::default();
        let node = json!({
            "type": "WWWWWWWWWWWWWWWWWWWWWWWW",
            "value": "https://WWWWWWWWWWWWWWWWWWWW.test/WWWWWWWWWWWWWWWWWWWW"
        });
        let output = context.run(Default::default(), |context| {
            egui::CentralPanel::default().show(context, |ui| {
                let painter = ui.painter().clone();
                draw_node(ui, &painter, Pos2::new(100.0, 100.0), &node, true, 0);
            });
        });
        let mut labels = 0;
        for clipped in output.shapes {
            if let egui::Shape::Text(text) = clipped.shape {
                labels += 1;
                assert!(text.galley.size().x <= 132.1);
                assert!(text.pos.x >= 34.0);
                assert!(text.pos.x + text.galley.size().x <= 166.1);
                assert_eq!(text.galley.rows.len(), 1);
            }
        }
        assert_eq!(labels, 3);
    }

    #[test]
    fn relationship_text_fits_narrow_label_and_escapes_controls() {
        let context = egui::Context::default();
        let rect = Rect::from_min_size(Pos2::new(50.0, 50.0), Vec2::new(67.0, 20.0));
        let output = context.run(Default::default(), |context| {
            egui::CentralPanel::default().show(context, |ui| {
                draw_label(ui.painter(), rect, "\u{202e}WWWWWWWWWWWWWWWW", 12.0, INK);
            });
        });
        let mut labels = 0;
        for clipped in output.shapes {
            if let egui::Shape::Text(text) = clipped.shape {
                labels += 1;
                assert!(text.galley.size().x <= rect.width() + 0.1);
                assert!(rect.contains(text.pos));
                assert!(text.pos.x + text.galley.size().x <= rect.right() + 0.1);
                assert_eq!(text.galley.rows.len(), 1);
                assert!(!text.galley.job.text.contains('\u{202e}'));
                assert!(text.galley.job.text.starts_with("\\u{202e}"));
            }
        }
        assert_eq!(labels, 1);
    }

    #[test]
    fn crypto_labels_keep_chain_and_both_hash_ends() {
        let value = format!("ethereum:0x{}deadbeef", "a".repeat(32));
        let node = json!({"type":"blockchain_address","value":value});
        assert_eq!(
            node_label(&node),
            ("0xaaaaaaaa…deadbeef".into(), "ethereum".into())
        );
        assert_eq!(node["value"], value);
    }
}
