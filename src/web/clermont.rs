//! The Clermont sample flat invitation: guest page and visit pass, salesperson
//! page, and the guest list. Seated event types only. Guests have no email
//! here, so nothing on these paths sends mail.
use super::*;
use chrono::Datelike;

#[derive(serde::Serialize)]
struct Slot {
    date: String,
    time: String,
    label: String,
    left: i64,
}

#[derive(serde::Serialize)]
struct Day {
    date: String,
    weekday: String,
    number: String,
    month: String,
    slots: Vec<Slot>,
}

struct Invite {
    token: String,
    name: String,
    phone: String,
    et_id: String,
    host: String,
    duration: i32,
    min_notice: i32,
    venue: String,
    max_party: i64,
}

struct Visit {
    id: String,
    start_at: String,
    party: i64,
    ics_token: String,
}

/// Indian mobile: 10 digits from 6-9, with or without +91 or a leading 0.
fn normalize_phone(raw: &str) -> Option<String> {
    let digits: String = raw.chars().filter(char::is_ascii_digit).collect();
    let local = match digits.len() {
        10 => digits.as_str(),
        11 => digits.strip_prefix('0')?,
        12 => digits.strip_prefix("91")?,
        _ => return None,
    };
    matches!(local.as_bytes()[0], b'6'..=b'9').then(|| local.to_string())
}

fn mask_phone(phone: &str) -> String {
    if phone.len() != 10 {
        return String::new();
    }
    format!("{}••• ••{}", &phone[..2], &phone[7..])
}

fn time_label(time: &str) -> String {
    NaiveTime::parse_from_str(time, "%H:%M")
        .map(|t| t.format("%-I:%M %P").to_string())
        .unwrap_or_else(|_| time.to_string())
}

async fn load_invite(pool: &SqlitePool, token: &str) -> Option<Invite> {
    let row: (String, String, String, String, i32, i32, String, i32) = sqlx::query_as(
        "SELECT i.guest_name, COALESCE(i.guest_phone, ''), i.event_type_id, u.name,
                et.duration_min, et.min_notice_min, COALESCE(et.location_value, ''),
                et.max_additional_guests
         FROM booking_invites i
         JOIN event_types et ON et.id = i.event_type_id
         JOIN users u ON u.id = i.created_by_user_id
         WHERE i.token = ? AND et.seats_per_slot > 1",
    )
    .bind(token)
    .fetch_optional(pool)
    .await
    .ok()??;
    Some(Invite {
        token: token.to_string(),
        name: row.0,
        phone: row.1,
        et_id: row.2,
        host: row.3,
        duration: row.4,
        min_notice: row.5,
        venue: row.6,
        max_party: i64::from(row.7) + 1,
    })
}

async fn load_visit(pool: &SqlitePool, token: &str) -> Option<Visit> {
    let row: (String, String, i64, String) = sqlx::query_as(
        "SELECT id, start_at, party_size, cancel_token FROM bookings
         WHERE invite_token = ? AND status = 'confirmed'",
    )
    .bind(token)
    .fetch_optional(pool)
    .await
    .ok()??;
    Some(Visit {
        id: row.0,
        start_at: row.1,
        party: row.2,
        ics_token: row.3,
    })
}

/// Every slot the event offers, with seats left. A full slot stays in the
/// list so the page can show it greyed out.
async fn open_days(state: &AppState, invite: &Invite, tz: Tz) -> Vec<Day> {
    let window = get_booking_window(&state.pool, &invite.et_id).await;
    let today = Utc::now().with_timezone(&tz).date_naive();
    let days_ahead = window
        .bookable_until
        .map_or(60, |until| (until - today).num_days() + 1)
        .clamp(1, 366) as i32;
    let days = compute_slots(
        &state.pool,
        &invite.et_id,
        invite.duration,
        0,
        0,
        invite.min_notice,
        0,
        days_ahead,
        window,
        tz,
        tz,
        BusySource::Individual(Vec::new()),
        None,
    )
    .await;
    let seats = crate::booking_time::seats_per_slot(&state.pool, &invite.et_id)
        .await
        .unwrap_or(1);
    let taken = crate::booking_time::seats_taken_by_start(&state.pool, &invite.et_id)
        .await
        .unwrap_or_default();
    days.into_iter()
        .filter_map(|day| {
            let date = NaiveDate::parse_from_str(&day.date, "%Y-%m-%d").ok()?;
            let slots = day
                .slots
                .into_iter()
                .map(|s| {
                    let start = slot_start(&s.host_date, &s.host_time, tz, invite.duration)
                        .map(|(start, _)| start)
                        .unwrap_or_default();
                    Slot {
                        label: time_label(&s.host_time),
                        left: (seats - taken.get(&start).copied().unwrap_or(0)).max(0),
                        date: s.host_date,
                        time: s.host_time,
                    }
                })
                .collect();
            Some(Day {
                weekday: date.format("%a").to_string(),
                number: date.day().to_string(),
                month: date.format("%b").to_string(),
                date: day.date,
                slots,
            })
        })
        .collect()
}

fn slot_start(date: &str, time: &str, tz: Tz, minutes: i32) -> Option<(String, String)> {
    let naive = NaiveDateTime::parse_from_str(&format!("{date} {time}"), "%Y-%m-%d %H:%M").ok()?;
    crate::booking_time::encode(naive, tz, minutes)
}

fn is_open(days: &[Day], date: &str, time: &str) -> bool {
    days.iter()
        .flat_map(|d| &d.slots)
        .any(|s| s.date == date && s.time == time)
}

fn page(state: &AppState, name: &str, ctx: minijinja::Value) -> Response {
    let rendered = state
        .templates
        .get_template(name)
        .and_then(|t| t.render(ctx));
    match rendered {
        Ok(html) => Html(html).into_response(),
        Err(e) => internal_error_response("template", &e),
    }
}

/// The invitation, or the visit pass once the guest has booked.
pub(super) async fn invitation(
    State(state): State<Arc<AppState>>,
    Path(token): Path<String>,
) -> Response {
    let Some(invite) = load_invite(&state.pool, &token).await else {
        return page(&state, "invitation.html", context! { invalid => true });
    };
    let tz = get_host_tz(&state.pool, &invite.et_id).await;
    let days = open_days(&state, &invite, tz).await;
    let days_json = serde_json::to_string(&days).unwrap_or_else(|_| "[]".to_string());
    let visit = load_visit(&state.pool, &token).await;
    let pass = visit.as_ref().and_then(|v| {
        let start = crate::booking_time::local(&v.start_at, tz, tz)?;
        Some(context! {
            date => start.format("%A %-d %B").to_string(),
            time => start.format("%-I:%M %P").to_string(),
            party => v.party,
            ics => format!("/booking/ics/{}", v.ics_token),
            can_change => start > Utc::now().with_timezone(&tz).naive_local()
                + Duration::minutes(invite.min_notice.into()),
        })
    });
    page(
        &state,
        "invitation.html",
        context! {
            token => invite.token,
            guest_name => invite.name,
            masked_phone => mask_phone(&invite.phone),
            host => invite.host,
            maps_url => format!(
                "https://www.google.com/maps/search/?api=1&query={}",
                urlencoding::encode(&invite.venue)
            ),
            venue => invite.venue,
            max_party => invite.max_party,
            days_json => days_json,
            pass => pass,
        },
    )
}

#[derive(Deserialize)]
pub(super) struct VisitForm {
    #[serde(default)]
    name: String,
    #[serde(default)]
    party_size: i64,
    #[serde(default)]
    others: String,
    date: String,
    time: String,
}

fn refused(message: &str) -> Response {
    axum::Json(serde_json::json!({ "ok": false, "error": message })).into_response()
}

fn done() -> Response {
    axum::Json(serde_json::json!({ "ok": true })).into_response()
}

const SLOT_GONE: &str = "That time has just filled up. Please choose another.";

pub(super) async fn book_visit(
    State(state): State<Arc<AppState>>,
    Path(token): Path<String>,
    Form(form): Form<VisitForm>,
) -> Response {
    let Some(invite) = load_invite(&state.pool, &token).await else {
        return refused("This invitation is not valid.");
    };
    let name = form.name.trim();
    if name.is_empty() || name.chars().count() > 100 {
        return refused("Please enter your name.");
    }
    if form.party_size < 1 || form.party_size > invite.max_party {
        return refused("Please choose the number of persons.");
    }
    if form.others.chars().count() > 500 {
        return refused("Please shorten the names of the others.");
    }
    let tz = get_host_tz(&state.pool, &invite.et_id).await;
    let Some((start, end)) = slot_start(&form.date, &form.time, tz, invite.duration) else {
        return refused(SLOT_GONE);
    };
    let days = open_days(&state, &invite, tz).await;
    let _turn = crate::resources::booking_lock().await;
    // A double tap arrives here twice; the second finds the first's booking.
    if load_visit(&state.pool, &token).await.is_some() {
        return done();
    }
    if !is_open(&days, &form.date, &form.time) {
        return refused(SLOT_GONE);
    }
    let seats = crate::booking_time::seats_per_slot(&state.pool, &invite.et_id)
        .await
        .unwrap_or(1);
    let taken = crate::booking_time::seats_taken(&state.pool, &invite.et_id, &start, "")
        .await
        .unwrap_or(seats);
    if taken + form.party_size > seats {
        return refused(SLOT_GONE);
    }
    let id = uuid::Uuid::new_v4().to_string();
    let inserted = sqlx::query(
        "INSERT INTO bookings (time_version, id, event_type_id, uid, guest_name, guest_email,
             guest_timezone, notes, start_at, end_at, status, cancel_token, reschedule_token,
             language, guest_phone, party_size, seat_key, invite_token)
         VALUES (1, ?, ?, ?, ?, '', ?, ?, ?, ?, 'confirmed', ?, ?, 'en', ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&invite.et_id)
    .bind(format!("{}@calrs", uuid::Uuid::new_v4()))
    .bind(name)
    .bind(tz.name())
    .bind(form.others.trim())
    .bind(&start)
    .bind(&end)
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(&invite.phone)
    .bind(form.party_size)
    .bind(&id)
    .bind(&token)
    .execute(&state.pool)
    .await;
    match inserted {
        Ok(_) => done(),
        Err(e) => {
            tracing::error!(error = %e, "clermont booking failed");
            refused("Something went wrong. Please try again.")
        }
    }
}

/// Moves the booking in one write, so it can never end up doubled or lost.
pub(super) async fn move_visit(
    State(state): State<Arc<AppState>>,
    Path(token): Path<String>,
    Form(form): Form<VisitForm>,
) -> Response {
    let Some(invite) = load_invite(&state.pool, &token).await else {
        return refused("This invitation is not valid.");
    };
    let tz = get_host_tz(&state.pool, &invite.et_id).await;
    let Some((start, end)) = slot_start(&form.date, &form.time, tz, invite.duration) else {
        return refused(SLOT_GONE);
    };
    let days = open_days(&state, &invite, tz).await;
    let _turn = crate::resources::booking_lock().await;
    let Some(visit) = load_visit(&state.pool, &token).await else {
        return refused("We could not find your visit.");
    };
    let visit_start = crate::booking_time::local(&visit.start_at, tz, tz);
    let cutoff =
        Utc::now().with_timezone(&tz).naive_local() + Duration::minutes(invite.min_notice.into());
    if visit_start.is_none_or(|s| s <= cutoff) {
        return refused("This visit can no longer be changed. Please call your host.");
    }
    if !is_open(&days, &form.date, &form.time) {
        return refused(SLOT_GONE);
    }
    let seats = crate::booking_time::seats_per_slot(&state.pool, &invite.et_id)
        .await
        .unwrap_or(1);
    let taken = crate::booking_time::seats_taken(&state.pool, &invite.et_id, &start, &visit.id)
        .await
        .unwrap_or(seats);
    if taken + visit.party > seats {
        return refused(SLOT_GONE);
    }
    let moved = sqlx::query(
        "UPDATE bookings SET start_at = ?, end_at = ?, ics_sequence = ics_sequence + 1,
                             reminder_sent_at = NULL
         WHERE id = ?",
    )
    .bind(&start)
    .bind(&end)
    .bind(&visit.id)
    .execute(&state.pool)
    .await;
    match moved {
        Ok(_) => done(),
        Err(e) => {
            tracing::error!(error = %e, "clermont move failed");
            refused("Something went wrong. Please try again.")
        }
    }
}

async fn seated_event(pool: &SqlitePool) -> Option<String> {
    sqlx::query_scalar("SELECT id FROM event_types WHERE seats_per_slot > 1 AND enabled = 1 LIMIT 1")
        .fetch_optional(pool)
        .await
        .ok()?
}

pub(super) async fn sales_page(
    State(state): State<Arc<AppState>>,
    auth: crate::auth::AuthUser,
) -> Response {
    let tz = match seated_event(&state.pool).await {
        Some(et_id) => get_host_tz(&state.pool, &et_id).await,
        None => Tz::UTC,
    };
    let rows: Vec<(String, String, String, Option<String>, Option<i64>, i64)> = sqlx::query_as(
        "SELECT i.token, i.guest_name, COALESCE(i.guest_phone, ''),
                (SELECT b.start_at FROM bookings b
                 WHERE b.invite_token = i.token AND b.status = 'confirmed'),
                (SELECT b.party_size FROM bookings b
                 WHERE b.invite_token = i.token AND b.status = 'confirmed'),
                EXISTS (SELECT 1 FROM bookings b
                        WHERE b.invite_token = i.token AND b.status = 'cancelled')
         FROM booking_invites i
         WHERE i.created_by_user_id = ? AND i.guest_phone IS NOT NULL
         ORDER BY i.created_at DESC",
    )
    .bind(&auth.user.id)
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();
    let invites: Vec<minijinja::Value> = rows
        .into_iter()
        .map(|(token, name, phone, start, party, cancelled)| {
            let when = start
                .and_then(|s| crate::booking_time::local(&s, tz, tz))
                .map(|s| s.format("%a %-d %b, %-I:%M %P").to_string());
            context! {
                token => token,
                name => name,
                phone => phone,
                when => when,
                party => party,
                cancelled => cancelled == 1,
            }
        })
        .collect();
    page(
        &state,
        "sales.html",
        context! { salesperson => auth.user.name, invites => invites },
    )
}

#[derive(Deserialize)]
pub(super) struct NewInvite {
    _csrf: Option<String>,
    phone: String,
    #[serde(default)]
    name: String,
}

pub(super) async fn create_invite(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    auth: crate::auth::AuthUser,
    Form(form): Form<NewInvite>,
) -> Response {
    if let Err(resp) = verify_csrf_token(&headers, &form._csrf) {
        return resp;
    }
    let Some(phone) = normalize_phone(&form.phone) else {
        return refused("Please enter a 10-digit mobile number.");
    };
    let Some(et_id) = seated_event(&state.pool).await else {
        return refused("The event is not set up yet.");
    };
    let name = form.name.trim();
    if name.chars().count() > 100 {
        return refused("That name is too long.");
    }
    let invited: Option<(String, String, String, String)> = sqlx::query_as(
        "SELECT i.token, i.guest_name, i.created_by_user_id, u.name FROM booking_invites i
         JOIN users u ON u.id = i.created_by_user_id
         WHERE i.event_type_id = ? AND i.guest_phone = ?",
    )
    .bind(&et_id)
    .bind(&phone)
    .fetch_optional(&state.pool)
    .await
    .unwrap_or(None);
    if let Some((token, existing_name, owner, owner_name)) = invited {
        if owner != auth.user.id {
            return refused(&format!("{owner_name} has already invited this guest."));
        }
        return axum::Json(serde_json::json!({
            "ok": true, "token": token, "phone": phone, "name": existing_name,
        }))
        .into_response();
    }
    let token = uuid::Uuid::new_v4().simple().to_string();
    let saved = sqlx::query(
        "INSERT INTO booking_invites
             (id, event_type_id, token, guest_name, guest_email, max_uses,
              created_by_user_id, guest_phone)
         VALUES (?, ?, ?, ?, '', 1, ?, ?)",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(&et_id)
    .bind(&token)
    .bind(name)
    .bind(&auth.user.id)
    .bind(&phone)
    .execute(&state.pool)
    .await;
    match saved {
        Ok(_) => axum::Json(serde_json::json!({
            "ok": true, "token": token, "phone": phone, "name": name,
        }))
        .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "clermont invite failed");
            refused("Something went wrong. Please try again.")
        }
    }
}

type GuestRow = (String, String, i64, String, String, String);

async fn guest_rows(pool: &SqlitePool, tz: Tz) -> Vec<(String, GuestRow)> {
    let rows: Vec<(String, String, String, i64, String, String)> = sqlx::query_as(
        "SELECT b.start_at, b.guest_name, COALESCE(b.guest_phone, ''), b.party_size,
                COALESCE(b.notes, ''), COALESCE(u.name, '')
         FROM bookings b
         JOIN event_types et ON et.id = b.event_type_id
         LEFT JOIN booking_invites i ON i.token = b.invite_token
         LEFT JOIN users u ON u.id = i.created_by_user_id
         WHERE et.seats_per_slot > 1 AND b.status = 'confirmed'
         ORDER BY b.start_at, b.created_at",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    rows.into_iter()
        .map(|(start, name, phone, party, notes, salesperson)| {
            let slot = crate::booking_time::local(&start, tz, tz)
                .map(|s| s.format("%a %-d %b, %-I:%M %P").to_string())
                .unwrap_or(start);
            (slot.clone(), (slot, name, party, phone, notes, salesperson))
        })
        .collect()
}

pub(super) async fn guests_page(
    State(state): State<Arc<AppState>>,
    _admin: crate::auth::AdminUser,
) -> Response {
    let tz = match seated_event(&state.pool).await {
        Some(et_id) => get_host_tz(&state.pool, &et_id).await,
        None => Tz::UTC,
    };
    let mut slots: Vec<minijinja::Value> = Vec::new();
    let mut current: Option<(String, Vec<minijinja::Value>, i64)> = None;
    for (slot, (_, name, party, phone, notes, salesperson)) in guest_rows(&state.pool, tz).await {
        if current.as_ref().is_none_or(|(s, _, _)| *s != slot) {
            if let Some((label, guests, total)) = current.take() {
                slots.push(context! { label => label, guests => guests, total => total });
            }
            current = Some((slot, Vec::new(), 0));
        }
        if let Some((_, guests, total)) = current.as_mut() {
            *total += party;
            guests.push(context! {
                name => name, party => party, phone => phone,
                others => notes, salesperson => salesperson,
            });
        }
    }
    if let Some((label, guests, total)) = current {
        slots.push(context! { label => label, guests => guests, total => total });
    }
    page(&state, "guests.html", context! { slots => slots })
}

/// Quoted for CSV, and a leading `=+-@` is neutralised so a guest's name can
/// never run as a formula when the sheet is opened.
fn csv_cell(value: &str) -> String {
    let safe = if value.starts_with(['=', '+', '-', '@']) {
        format!("'{value}")
    } else {
        value.to_string()
    };
    format!("\"{}\"", safe.replace('"', "\"\""))
}

pub(super) async fn guests_csv(
    State(state): State<Arc<AppState>>,
    _admin: crate::auth::AdminUser,
) -> Response {
    let tz = match seated_event(&state.pool).await {
        Some(et_id) => get_host_tz(&state.pool, &et_id).await,
        None => Tz::UTC,
    };
    let mut csv = String::from("Slot,Guest,Persons,Mobile,Others,Salesperson\n");
    for (_, (slot, name, party, phone, notes, salesperson)) in guest_rows(&state.pool, tz).await {
        let cells = [
            csv_cell(&slot),
            csv_cell(&name),
            party.to_string(),
            csv_cell(&phone),
            csv_cell(&notes),
            csv_cell(&salesperson),
        ];
        csv.push_str(&cells.join(","));
        csv.push('\n');
    }
    (
        [
            (axum::http::header::CONTENT_TYPE, "text/csv; charset=utf-8"),
            (
                axum::http::header::CONTENT_DISPOSITION,
                "attachment; filename=\"clermont-guests.csv\"",
            ),
        ],
        csv,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indian_mobiles_normalize() {
        for raw in ["9876543210", "+91 98765 43210", "09876543210", "919876543210"] {
            assert_eq!(normalize_phone(raw).as_deref(), Some("9876543210"));
        }
        for raw in ["", "12345", "1234567890", "5876543210", "19876543210"] {
            assert_eq!(normalize_phone(raw), None);
        }
    }

    #[test]
    fn phones_are_masked() {
        assert_eq!(mask_phone("9876543210"), "98••• ••210");
        assert_eq!(mask_phone(""), "");
    }

    #[test]
    fn csv_cells_cannot_become_formulas() {
        assert_eq!(csv_cell("=SUM(A1)"), "\"'=SUM(A1)\"");
        assert_eq!(csv_cell("a\"b"), "\"a\"\"b\"");
    }
}
