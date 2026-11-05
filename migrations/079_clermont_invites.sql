-- Invitations go to a mobile number; a booking remembers its invitation.
ALTER TABLE booking_invites ADD COLUMN guest_phone TEXT;
ALTER TABLE bookings ADD COLUMN invite_token TEXT;
CREATE UNIQUE INDEX idx_invites_one_per_phone
ON booking_invites(event_type_id, guest_phone) WHERE guest_phone IS NOT NULL;
CREATE UNIQUE INDEX idx_bookings_one_per_invite
ON bookings(invite_token) WHERE invite_token IS NOT NULL AND status = 'confirmed';
