CREATE INDEX IF NOT EXISTS messages_conversation_order
    ON messages (conversation_id, id DESC);
