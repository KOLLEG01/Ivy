export type WhatsAppLanguage = "en" | "de";
export type MessageParameters = Readonly<Record<string, string | number>>;

const english = {
  help: `IvyChat – one shared Main
/status · /task – Main and settings
/new – create a new Main (does not select a task)
/models · /model <id|default> [effort]
/reasoning <effort|default> · /fast on|off
/commentary on|off
/steer <text> – add to the running Main
/threads [1–25] – read-only overview
/desktop ensure|restart
/help`,
  "command.invalid": "Invalid command.",
  "command.failed": "Command could not be confirmed: {code}{detail}",
  "new.usage":
    "/new only creates a new Main; an existing task cannot be selected.",
  "new.reason": "Explicit WhatsApp /new command",
  "new.failed": "Main could not be replaced: {code}",
  "new.success": "New Main: {id}",
  "steer.usage": "Usage: /steer <text>",
  "steer.first": "Use /new first.",
  "steer.inactive": "No active Main turn. /steer does not start a new turn.",
  "steer.changed": "Main changed before steering.",
  "steer.success": "Added to the running Main.",
  "task.status_only": "/task only shows status; it does not select a task.",
  "status.pending": "created with the first message",
  "status.default": "default",
  "status.enabled": "on",
  "status.disabled": "off",
  status:
    "Main: {main}\nQueue: {queue}\nModel: {model}\nReasoning: {reasoning}\nFast: {fast}\nCommentary: {commentary}",
  "toggle.usage": "Usage: /{name} on|off",
  "toggle.fast": "Fast Mode (from the next WhatsApp turn)",
  "toggle.commentary": "Commentary",
  "toggle.result": "{setting}: {state}.",
  "reasoning.invalid": "Invalid reasoning value.",
  "reasoning.result": "Reasoning from the next WhatsApp turn: {effort}",
  "models.missing": "Model catalog is missing.",
  "model.usage": "Usage: /model <id|default> [effort]",
  "model.unavailable": "This model is not available in the native catalog.",
  "model.reasoning": "This model does not support that reasoning effort.",
  "model.result": "Model from the next WhatsApp turn: {model}",
  "threads.usage": "Usage: /threads [1–25]",
  "threads.header": "For information only; no task selection:",
  "command.unknown":
    "Unknown command. There is only one Main; /new creates a new one.\n{help}",
  "main.pending": "Automatic Main creation has not finished yet.",
  "main.first_message": "Main is created automatically with the first message.",
  "main.unavailable": "The native Main session is unavailable. Use /new to create a new Main, then send your message again.",
  "message.rejected": "Message could not be accepted: {code}",
  "message.expired":
    "The outcome of this request is unclear. Check Main with /status; the message will not be run again automatically.",
  "message.failed": "Message was not processed: {code}",
  "message.auth_required": "Main needs a valid Codex sign-in on its host. Your message is saved and will continue automatically after sign-in.",
  "media.declared_limit": "The attachment exceeds the size limit ({mib} MiB).",
  "media.too_large": "The attachment is too large.",
  "media.empty": "The file is empty.",
  "media.checksum": "The attachment does not match its original checksum.",
  "media.limit": "The attachment exceeds the size limit.",
  "media.image_types": "Supported images: PNG, JPEG, WebP.",
  "media.image_caption": "Image from WhatsApp",
  "media.transcription_missing":
    "Voice messages require configured transcription access.",
  "media.transcription_prefix":
    "Voice message (automatically transcribed):\n{transcript}",
  "media.voice_declared_limit":
    "Voice messages may be at most ten minutes long.",
  "media.voice_actual_limit": "The actual audio duration exceeds ten minutes.",
  "media.transcription_https": "Transcription requires HTTPS.",
  "media.transcription_failed": "Transcription failed.",
  "media.transcription_invalid":
    "The transcription is empty, incomplete, or too long.",
  "media.document_host": "Documents require ChatBridge on the Main host.",
  "media.original_changed": "The original file was changed.",
  "media.attachment": "File attachment: {name}",
  "media.supported": "Send text, an image, a document, or a voice message.",
  "media.message_limit": "The message is too long.",
  "audio.timeout": "The voice message could not be decoded in time.",
  "audio.invalid": "The voice message could not be decoded.",
  "audio.incomplete": "The voice message was not decoded completely.",
  "audio.too_long": "Invalid or overly long voice message.",
  "desktop.usage": "Usage: /desktop ensure|restart",
  "desktop.platform": "Desktop commands require the Windows host.",
  "desktop.unclear":
    "The desktop command had an unclear outcome and will not be repeated.",
  "desktop.restart":
    "Desktop was closed gracefully and its start was requested. Main is unchanged.",
  "desktop.ensure":
    "Desktop start was checked/requested. Main runs independently in AgentManager.",
  "native.previous_unclear": "The outcome of the earlier request is unclear.",
  "native.previous_failed": "The earlier request failed.",
  "listener.created": "Task created.",
  "listener.task": "Task {id}",
  "listener.terminal": "Main turn ended: {status}. No final text response.",
  "listener.failed": "Main could not answer: {reason}",
  "listener.auth_required": "Main could not answer because its Codex sign-in has expired or is invalid. Sign in again on the Main host, then resend this message.",
} as const;

export type MessageKey = keyof typeof english;
const german: Partial<Record<MessageKey, string>> = {
  help: `IvyChat – ein gemeinsamer Main
/status · /task – Main und Einstellungen
/new – neuen Main erstellen (kein Task-Wechsel)
/models · /model <id|default> [effort]
/reasoning <effort|default> · /fast on|off
/commentary on|off
/steer <text> – laufenden Main ergänzen
/threads [1–25] – nur lesende Übersicht
/desktop ensure|restart
/help`,
  "command.invalid": "Ungültiger Befehl.",
  "command.failed": "Befehl konnte nicht bestätigt werden: {code}{detail}",
  "new.usage":
    "/new erstellt nur einen neuen Main; ein bestehender Task kann nicht ausgewählt werden.",
  "new.reason": "Expliziter WhatsApp-Befehl /new",
  "new.failed": "Main konnte nicht ersetzt werden: {code}",
  "new.success": "Neuer Main: {id}",
  "steer.usage": "Verwendung: /steer <text>",
  "steer.first": "Zuerst /new verwenden.",
  "steer.inactive": "Kein aktiver Main-Turn. /steer startet keinen neuen Turn.",
  "steer.changed": "Main hat sich vor dem Steering geändert.",
  "steer.success": "Ergänzung an den laufenden Main übergeben.",
  "task.status_only":
    "/task ist nur eine Statusanzeige; es gibt keine Task-Auswahl.",
  "status.pending": "wird mit der ersten Nachricht erstellt",
  "status.default": "Standard",
  "status.enabled": "an",
  "status.disabled": "aus",
  status:
    "Main: {main}\nQueue: {queue}\nModell: {model}\nReasoning: {reasoning}\nFast: {fast}\nCommentary: {commentary}",
  "toggle.usage": "Verwendung: /{name} on|off",
  "toggle.fast": "Fast Mode (ab nächstem WhatsApp-Turn)",
  "toggle.commentary": "Commentary",
  "toggle.result": "{setting}: {state}.",
  "reasoning.invalid": "Ungültiger Reasoning-Wert.",
  "reasoning.result": "Reasoning ab nächstem WhatsApp-Turn: {effort}",
  "models.missing": "Modellkatalog fehlt.",
  "model.usage": "Verwendung: /model <id|default> [effort]",
  "model.unavailable": "Dieses Modell ist im nativen Katalog nicht verfügbar.",
  "model.reasoning": "Das Modell unterstützt dieses Reasoning nicht.",
  "model.result": "Modell ab nächstem WhatsApp-Turn: {model}",
  "threads.usage": "Verwendung: /threads [1–25]",
  "threads.header": "Nur Information; keine Task-Auswahl:",
  "command.unknown":
    "Unbekannter Befehl. Es gibt nur einen Main; /new erstellt einen neuen.\n{help}",
  "main.pending":
    "Die automatische Main-Erstellung ist noch nicht abgeschlossen.",
  "main.first_message":
    "Main wird mit der ersten Nachricht automatisch erstellt.",
  "message.rejected": "Nachricht konnte nicht angenommen werden: {code}",
  "message.expired":
    "Der Ausgang dieses Aufrufs ist unklar. Bitte den Main mit /status prüfen; die Nachricht wird nicht automatisch erneut ausgeführt.",
  "message.failed": "Nachricht nicht verarbeitet: {code}",
  "message.auth_required": "Main benötigt eine gültige Codex-Anmeldung auf seinem Host. Deine Nachricht ist gespeichert und wird nach der Anmeldung automatisch verarbeitet.",
  "media.declared_limit":
    "Der Anhang überschreitet das Größenlimit ({mib} MiB).",
  "media.too_large": "Anhang zu groß.",
  "media.empty": "Die Datei ist leer.",
  "media.checksum":
    "Der Anhang stimmt nicht mit seiner ursprünglichen Prüfsumme überein.",
  "media.limit": "Der Anhang überschreitet das Größenlimit.",
  "media.image_types": "Unterstützte Bilder: PNG, JPEG, WebP.",
  "media.image_caption": "Bild aus WhatsApp",
  "media.transcription_missing":
    "Sprachnachrichten benötigen den konfigurierten Transkriptionszugang.",
  "media.transcription_prefix":
    "Sprachnachricht (automatisch transkribiert):\n{transcript}",
  "media.voice_declared_limit":
    "Sprachnachrichten dürfen höchstens zehn Minuten lang sein.",
  "media.voice_actual_limit":
    "Die tatsächliche Audiodauer überschreitet zehn Minuten.",
  "media.transcription_https": "Transkription benötigt HTTPS.",
  "media.transcription_failed": "Die Transkription ist fehlgeschlagen.",
  "media.transcription_invalid":
    "Die Transkription ist leer, unvollständig oder zu lang.",
  "media.document_host": "Dokumente benötigen ChatBridge auf dem Main-Host.",
  "media.original_changed": "Originaldatei wurde verändert.",
  "media.attachment": "Dateianhang: {name}",
  "media.supported":
    "Bitte Text, ein Bild, ein Dokument oder eine Sprachnachricht senden.",
  "media.message_limit": "Die Nachricht ist zu lang.",
  "audio.timeout": "Sprachnachricht konnte nicht rechtzeitig dekodiert werden.",
  "audio.invalid": "Sprachnachricht konnte nicht dekodiert werden.",
  "audio.incomplete": "Sprachnachricht wurde nicht vollständig dekodiert.",
  "audio.too_long": "Ungültige oder zu lange Sprachnachricht.",
  "desktop.usage": "Verwendung: /desktop ensure|restart",
  "desktop.unclear":
    "Der Desktop-Aufruf hatte einen unklaren Ausgang und wird nicht wiederholt.",
  "desktop.restart":
    "Desktop wurde geordnet geschlossen; sein Start wurde angefordert. Main bleibt unverändert.",
  "desktop.ensure":
    "Desktop-Start geprüft/angefordert. Main läuft unabhängig im AgentManager.",
  "native.previous_unclear": "Der Ausgang des früheren Aufrufs ist unklar.",
  "native.previous_failed": "Der frühere Aufruf ist fehlgeschlagen.",
  "listener.created": "Task angelegt.",
  "listener.task": "Task {id}",
  "listener.terminal": "Main-Turn beendet: {status}. Keine finale Textantwort.",
  "listener.failed": "Main konnte nicht antworten: {reason}",
  "listener.auth_required": "Main konnte nicht antworten, weil seine Codex-Anmeldung abgelaufen oder ungültig ist. Bitte auf dem Main-Host erneut anmelden und diese Nachricht anschließend erneut senden.",
};

export type Translator = (
  key: MessageKey,
  parameters?: MessageParameters,
) => string;
export function translator(language: WhatsAppLanguage = "en"): Translator {
  return (key, parameters = {}) =>
    ((language === "de" ? german[key] : undefined) ?? english[key]).replace(
      /\{([a-z]+)\}/gi,
      (placeholder, name) => {
        const value = parameters[name];
        return value === undefined ? placeholder : String(value);
      },
    );
}
