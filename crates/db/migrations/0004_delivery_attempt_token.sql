-- Reserve an uncertain recipient before SMTP side effects. Only this lease can
-- resolve it; rollback/crash leaves ambiguous rather than replayable pending.
ALTER TABLE delivery_recipients ADD COLUMN attempt_token TEXT;
