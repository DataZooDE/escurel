//! The supplier-risk ANALYSIS the echo harness writes beside its fold (demo data).
//!
//! A supplier-risk run does not only append a line to the sales order: it persists what it worked
//! out as a `supplier-risk-analysis` instance, deterministically from the signal, the supplier and
//! the orders that depend on the supplier's material. The page carries typed fields, a findings
//! section, the affected-orders table, and for the chart a TEXT ALTERNATIVE: one plain sentence
//! stating the takeaway and the table behind it, so an agent or reader that sees only the markdown
//! (no Peacock) still learns what the graph shows.

/// What the purchasing signal says.
#[derive(Debug, PartialEq)]
pub struct Signal {
    pub vendor: String,
    pub material: Option<String>,
    pub days_moved: u32,
    pub downgraded: bool,
}

/// The word after `key` (case-insensitive) up to the next character that is not one of `ok`.
fn word_after(text: &str, key: &str, ok: impl Fn(char) -> bool) -> Option<String> {
    let at = text.to_lowercase().find(&key.to_lowercase())? + key.len();
    let w: String = text[at..].chars().take_while(|c| ok(*c)).collect();
    (!w.is_empty()).then_some(w)
}

pub fn parse_signal(title: &str, body: &str) -> Option<Signal> {
    let text = format!("{title} {body}");
    let vendor = word_after(&text, "vendor ", |c| c.is_ascii_digit()).filter(|v| v.len() >= 4)?;
    let material = word_after(&text, "material ", |c| {
        c.is_ascii_alphanumeric() || c == '-'
    });
    let days_moved = word_after(&text, "+", |c| c.is_ascii_digit())
        .filter(|_| text.contains(" days"))
        .and_then(|d| d.parse().ok())
        .unwrap_or(0);
    Some(Signal {
        vendor,
        material,
        days_moved,
        downgraded: text.contains("downgraded"),
    })
}

/// One order item that depends on the supplier's material.
#[derive(Debug, PartialEq, Clone)]
pub struct OrderLine {
    pub order_id: String,
    pub customer: String,
    pub currency: String,
    pub qty: u64,
    pub net_value: f64,
}

fn number(cell: &str) -> Option<f64> {
    cell.replace(',', "").trim().parse().ok()
}

/// The item rows of `material` in an order body's items table (`| Item | Material | … | Qty | Unit |
/// Net value | … |`), as the lines of that order that depend on the supplier.
pub fn order_lines(
    order_id: &str,
    customer: &str,
    currency: &str,
    body: &str,
    material: &str,
) -> Vec<OrderLine> {
    body.lines()
        .filter(|l| l.trim_start().starts_with('|'))
        .filter_map(|l| {
            let cells: Vec<&str> = l
                .trim()
                .trim_matches('|')
                .split('|')
                .map(str::trim)
                .collect();
            (cells.len() >= 6 && cells[1] == material).then(|| OrderLine {
                order_id: order_id.to_owned(),
                customer: customer.to_owned(),
                currency: currency.to_owned(),
                qty: number(cells[3]).unwrap_or(0.0) as u64,
                net_value: number(cells[5]).unwrap_or(0.0),
            })
        })
        .collect()
}

pub struct Supplier {
    /// The supplier instance id (`meier-guss`).
    pub id: String,
    pub name: String,
    pub vendor: String,
}

pub struct Built {
    /// `markdown/instances/supplier-risk-analysis__<supplier>-<tail>.md`
    pub page_id: String,
    pub id: String,
    pub content: String,
}

/// A plain, explainable score: 5 points per day the confirmation moved, 20 for a rating downgrade.
pub fn risk(days_moved: u32, downgraded: bool) -> (&'static str, u32) {
    let score = (days_moved * 5 + if downgraded { 20 } else { 0 }).min(100);
    let level = match score {
        60.. => "high",
        30..=59 => "medium",
        _ => "low",
    };
    (level, score)
}

/// `62400` -> `62,400.00`.
fn money(v: f64) -> String {
    let fixed = format!("{v:.2}");
    let (int, frac) = fixed.split_once('.').unwrap_or((&fixed, "00"));
    let mut out = String::new();
    for (i, c) in int.chars().enumerate() {
        if i > 0 && (int.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    format!("{out}.{frac}")
}

/// The UTC day (`YYYY-MM-DD`) an event ULID was minted: its first ten characters are a millisecond
/// timestamp in Crockford base32. `None` for anything that is not a ULID.
pub fn ulid_date(event_id: &str) -> Option<String> {
    const ALPHABET: &str = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    if event_id.len() != 26 {
        return None;
    }
    let mut ms: u64 = 0;
    for c in event_id.chars().take(10) {
        let v = ALPHABET.find(c.to_ascii_uppercase())? as u64;
        ms = ms.checked_mul(32)?.checked_add(v)?;
    }
    // Days since 1970-01-01 to a civil date (Hinnant's algorithm).
    let z = (ms / 86_400_000) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    Some(format!("{y:04}-{m:02}-{d:02}"))
}

/// A human id for the analysis: the supplier and the day, with a counter when that day already has
/// one (`-2`, `-3`, ...). Stable (the same event on the same day gives the same id), readable, and
/// ids of one supplier sort by date. `taken` says whether a candidate id already exists.
pub fn analysis_id(supplier_id: &str, event_id: &str, taken: impl Fn(&str) -> bool) -> String {
    let base = match ulid_date(event_id) {
        Some(day) => format!("{supplier_id}-{day}"),
        None => supplier_id.to_owned(),
    };
    if !taken(&base) {
        return base;
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|c| !taken(c))
        .expect("an unbounded counter always finds a free id")
}

pub fn build_analysis(
    supplier: &Supplier,
    signal: &Signal,
    lines: &[OrderLine],
    event_id: &str,
    id: &str,
    trigger_title: &str,
) -> Built {
    let page_id = format!("markdown/instances/supplier-risk-analysis__{id}.md");
    let (level, score) = risk(signal.days_moved, signal.downgraded);
    let currency = lines.first().map_or("EUR", |l| l.currency.as_str());
    let total: f64 = lines.iter().map(|l| l.net_value).sum();
    let share = |v: f64| {
        if total > 0.0 {
            (v / total * 100.0).round() as u32
        } else {
            0
        }
    };

    let mut fm = format!(
        "---\nkind: instance\nskill: supplier-risk-analysis\nid: {id}\nsupplier: \"[[supplier::{}]]\"\nvendor: {}\n",
        supplier.id, supplier.vendor
    );
    if let Some(m) = &signal.material {
        fm.push_str(&format!("material: {m}\n"));
    }
    fm.push_str(&format!(
        "risk_level: {level}\nrisk_score: {score}\ndays_moved: {}\norders_affected: {}\nnet_value_at_risk: {total:.2}\ncurrency: {currency}\nsource_event: {event_id}\n---\n",
        signal.days_moved,
        lines.len()
    ));

    let takeaway = match lines
        .iter()
        .max_by(|a, b| a.net_value.total_cmp(&b.net_value))
    {
        None => "No order depends on this supplier's material.".to_owned(),
        Some(_) if lines.len() == 1 => format!(
            "1 order is affected and carries {} {currency} of net value.",
            money(total)
        ),
        Some(top) => format!(
            "{} orders are affected and carry {} {currency} of net value; the largest, {} ({}), is {}% of it.",
            lines.len(),
            money(total),
            top.order_id,
            top.customer,
            share(top.net_value)
        ),
    };

    let mut body = format!("\n# Supplier risk: {}\n\n", supplier.name);
    body.push_str(&format!(
        "**Supplier** [[supplier::{}]] · vendor {}",
        supplier.id, supplier.vendor
    ));
    if let Some(m) = &signal.material {
        body.push_str(&format!(" · material {m}"));
    }
    body.push_str(&format!("\n\n**Triggered by** {trigger_title}\n"));
    body.push_str("\n## Findings\n\n");
    body.push_str(&format!("- Risk **{level}** (score {score} of 100).\n"));
    if signal.days_moved > 0 {
        body.push_str(&format!(
            "- The vendor's confirmation moved by {} days.\n",
            signal.days_moved
        ));
    }
    if signal.downgraded {
        body.push_str("- The vendor's rating was downgraded.\n");
    }
    body.push_str(&format!(
        "- {} order(s) depend on the affected material.\n\n",
        lines.len()
    ));
    body.push_str(&format!("## Net value at risk per order\n\n{takeaway}\n\n"));
    body.push_str(&format!(
        "| Order | Customer | Qty | Net value ({currency}) | Share |\n|---|---|---:|---:|---:|\n"
    ));
    for l in lines {
        body.push_str(&format!(
            "| [[customer-order::{}]] | {} | {} | {} | {}% |\n",
            l.order_id,
            l.customer,
            l.qty,
            money(l.net_value),
            share(l.net_value)
        ));
    }
    body.push_str("\n## Follow-ups\n\nNotify the affected customers, or ask the supplier for a new confirmation. The buttons on this page start them.\n");

    Built {
        page_id,
        id: id.to_owned(),
        content: format!("{fm}{body}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TITLE: &str = "Vendor 100234 Meier-Guss: PO 4500087412 confirmation moved +14 days";
    const BODY: &str = "Purchasing (ME23N): vendor 100234 Meier-Guss GmbH, PO 4500087412 item 10, \
        material GH-4711 (gearbox housing), 240 PC. Confirmation date moved from 2026-10-12 to \
        2026-10-26 (+14 days); vendor rating downgraded A to B. Affects sales order 4500123.";

    fn supplier() -> Supplier {
        Supplier {
            id: "meier-guss".into(),
            name: "Meier-Guss GmbH".into(),
            vendor: "100234".into(),
        }
    }
    fn lines() -> Vec<OrderLine> {
        vec![
            OrderLine {
                order_id: "order-4500123".into(),
                customer: "Hoffmann Automotive GmbH".into(),
                currency: "EUR".into(),
                qty: 240,
                net_value: 62400.0,
            },
            OrderLine {
                order_id: "order-4500131".into(),
                customer: "Kessler Werkzeugbau GmbH".into(),
                currency: "EUR".into(),
                qty: 200,
                net_value: 66200.0,
            },
        ]
    }

    #[test]
    fn reads_vendor_material_delay_and_downgrade_from_the_signal() {
        assert_eq!(
            parse_signal(TITLE, BODY),
            Some(Signal {
                vendor: "100234".into(),
                material: Some("GH-4711".into()),
                days_moved: 14,
                downgraded: true
            })
        );
    }

    #[test]
    fn a_signal_without_a_vendor_is_not_analysable() {
        assert_eq!(parse_signal("lunch", "nothing about suppliers"), None);
    }

    #[test]
    fn a_partial_confirmation_has_no_delay_and_no_downgrade() {
        let s = parse_signal(
            "Vendor 100234 Meier-Guss: PO 4500087433 partially confirmed",
            "vendor 100234, material GH-4711: 120 of 200 PC confirmed",
        )
        .unwrap();
        assert_eq!((s.days_moved, s.downgraded), (0, false));
    }

    #[test]
    fn picks_the_item_rows_of_the_material_out_of_an_order_body() {
        let body = "# Sales order\n\n| Item | Material | Description | Qty | Unit | Net value | Confirmed |\n|---|---|---|---:|---|---:|---|\n| 10 | TH-0815 | Tool holder | 80 | PC | 31,100.00 | 2026-10-19 |\n| 20 | GH-4711 | Gearbox housing | 200 | PC | 66,200.00 | 2026-10-19 |\n";
        let got = order_lines("order-4500131", "Kessler", "EUR", body, "GH-4711");
        assert_eq!(got.len(), 1);
        assert_eq!((got[0].qty, got[0].net_value), (200, 66200.0));
        assert!(order_lines("order-4500131", "Kessler", "EUR", body, "XX-0000").is_empty());
    }

    #[test]
    fn risk_grows_with_the_delay_and_a_downgrade() {
        assert_eq!(risk(14, true), ("high", 90));
        assert_eq!(risk(7, false), ("medium", 35));
        assert_eq!(risk(0, false), ("low", 0));
    }

    // ULID 01M3YS0X30T80HN5HEH5J73X56: its first ten characters are a millisecond timestamp.
    const EVENT: &str = "01M3YS0X30T80HN5HEH5J73X56";

    #[test]
    fn the_day_comes_out_of_the_event_ulid() {
        // 2026-10-03T00:00:00Z is 1_790_985_600_000 ms; 01K... style ids of that day start 01M3Y/01M3Z.
        assert_eq!(
            ulid_date("01ARZ3NDEKTSV4RRFFQ69G5FAV").as_deref(),
            Some("2016-07-30")
        );
        assert_eq!(ulid_date("not-a-ulid"), None);
        assert_eq!(ulid_date("01ARZ3NDE"), None);
    }

    #[test]
    fn a_human_id_is_the_supplier_and_the_day_not_a_random_tail() {
        assert_eq!(
            analysis_id("meier-guss", "01ARZ3NDEKTSV4RRFFQ69G5FAV", |_| false),
            "meier-guss-2016-07-30"
        );
    }

    #[test]
    fn a_second_analysis_the_same_day_gets_a_counter_and_ids_sort() {
        let taken = ["meier-guss-2016-07-30", "meier-guss-2016-07-30-2"];
        let id = analysis_id("meier-guss", "01ARZ3NDEKTSV4RRFFQ69G5FAV", |c| {
            taken.contains(&c)
        });
        assert_eq!(id, "meier-guss-2016-07-30-3");
        assert!("meier-guss-2016-07-30" < "meier-guss-2016-07-31");
    }

    #[test]
    fn the_analysis_is_a_flat_instance_with_the_given_id() {
        let b = build_analysis(
            &supplier(),
            &parse_signal(TITLE, BODY).unwrap(),
            &lines(),
            EVENT,
            "meier-guss-2026-10-03",
            TITLE,
        );
        assert_eq!(b.id, "meier-guss-2026-10-03");
        assert_eq!(
            b.page_id,
            "markdown/instances/supplier-risk-analysis__meier-guss-2026-10-03.md"
        );
        assert!(b.content.starts_with(
            "---\nkind: instance\nskill: supplier-risk-analysis\nid: meier-guss-2026-10-03\n"
        ));
    }

    #[test]
    fn the_body_says_which_signal_it_answers_in_words_and_keeps_the_event_id_as_provenance() {
        let b = build_analysis(
            &supplier(),
            &parse_signal(TITLE, BODY).unwrap(),
            &lines(),
            EVENT,
            "meier-guss-2026-10-03",
            TITLE,
        );
        assert!(
            b.content.contains(&format!("**Triggered by** {TITLE}")),
            "{}",
            b.content
        );
        assert!(b.content.contains(&format!("source_event: {EVENT}\n")));
    }

    #[test]
    fn it_carries_typed_fields_for_what_it_found() {
        let b = build_analysis(
            &supplier(),
            &parse_signal(TITLE, BODY).unwrap(),
            &lines(),
            EVENT,
            "meier-guss-2026-10-03",
            TITLE,
        );
        for line in [
            "supplier: \"[[supplier::meier-guss]]\"",
            "vendor: 100234",
            "risk_level: high",
            "risk_score: 90",
            "days_moved: 14",
            "orders_affected: 2",
            "net_value_at_risk: 128600.00",
            "currency: EUR",
            "source_event: 01M3YS0X30T80HN5HEH5J73X56",
        ] {
            assert!(
                b.content.contains(&format!("\n{line}\n")),
                "missing `{line}` in\n{}",
                b.content
            );
        }
    }

    #[test]
    fn the_chart_has_a_text_alternative_a_takeaway_sentence_and_the_table_behind_it() {
        let b = build_analysis(
            &supplier(),
            &parse_signal(TITLE, BODY).unwrap(),
            &lines(),
            EVENT,
            "meier-guss-2026-10-03",
            TITLE,
        );
        // The takeaway, in words an agent that never renders a chart can use.
        assert!(
            b.content.contains("2 orders are affected and carry 128,600.00 EUR of net value; the largest, order-4500131 (Kessler Werkzeugbau GmbH), is 51% of it."),
            "{}", b.content
        );
        // The data table the chart is drawn from.
        assert!(
            b.content
                .contains("| Order | Customer | Qty | Net value (EUR) | Share |")
        );
        assert!(b.content.contains("| [[customer-order::order-4500123]] | Hoffmann Automotive GmbH | 240 | 62,400.00 | 49% |"));
        // The chart section is titled, and the sentence sits directly under the title.
        let at = b
            .content
            .find("## Net value at risk per order")
            .expect("chart section");
        assert!(b.content[at..].contains("\n\n2 orders are affected"));
    }

    #[test]
    fn it_links_the_supplier_and_is_deterministic() {
        let s = parse_signal(TITLE, BODY).unwrap();
        let a = build_analysis(
            &supplier(),
            &s,
            &lines(),
            EVENT,
            "meier-guss-2026-10-03",
            TITLE,
        );
        let b = build_analysis(
            &supplier(),
            &s,
            &lines(),
            EVENT,
            "meier-guss-2026-10-03",
            TITLE,
        );
        assert_eq!(a.content, b.content);
        assert!(a.content.contains("[[supplier::meier-guss]]"));
    }
}
