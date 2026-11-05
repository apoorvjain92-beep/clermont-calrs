-- Group sessions: a slot takes bookings until its seats run out.
ALTER TABLE event_types ADD COLUMN seats_per_slot INTEGER NOT NULL DEFAULT 1;
ALTER TABLE bookings ADD COLUMN party_size INTEGER NOT NULL DEFAULT 1;
-- '' keeps whole-slot exclusivity; a seated booking stores its own id here.
ALTER TABLE bookings ADD COLUMN seat_key TEXT NOT NULL DEFAULT '';
DROP INDEX IF EXISTS idx_bookings_no_overlap;
CREATE UNIQUE INDEX idx_bookings_no_overlap
ON bookings(event_type_id, start_at, COALESCE(assigned_user_id, ''), seat_key)
WHERE status IN ('confirmed', 'pending');
