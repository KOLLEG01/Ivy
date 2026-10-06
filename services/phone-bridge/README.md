# PhoneBridge

PhoneBridge verbindet SIP-Anrufe, Windows-Audio und Codex Voice.
[Voice-Anforderungen](../../specs/PHONE-MICRO.md) beschreiben die Bindung an Desktop
und den Lebenszyklus eines Gesprächs; die [Phone-Schemas](../../specs/schemas)
definieren Einstellungen, Operationen und Grenzwerte.

## Einrichtung

- Codex-Micro-Onboarding abschließen und die Mikrofontaste in Codex auf **Realtime**
  stellen. Die Emulation ändert diese App-Einstellung nicht.
- Aufnahme- und Wiedergabegeräte gemäß der tatsächlichen virtuellen Kabelverbindung
  auswählen. [Audio-Setup](src/runtime/audio-setup.ts) und
  [Hardware-Setup](../../tools/operations/setup-phone-audio-hardware.mjs) prüfen die Geräte.
- SIP-Zugang, optionale Zugangscode-Abfragen und Screening über die geschützte
  Instanzkonfiguration einrichten; maßgeblich sind [Settings](src/runtime/settings.ts)
  und [Admission](src/runtime/admission.ts). Treibereinrichtung erfolgt separat.

`phone.status` zeigt Konfiguration, Geräte und Micro-Handshake.
Ein bereiter Protokollkanal bestätigt noch kein aktives Voice-Gespräch.
`phone.history`, `phone.operation` und `phone.logs` zeigen aufbewahrte Anrufe,
ursprüngliche Ergebnisse und begrenzte Diagnosen.
PhoneBridge verwendet bei jeder neuen App-Tools-Verbindung die neueste vollständig
installierte Version aus dem lokalen Codex-Plugin-Cache. Gespeicherte Dateipfade
und SHA-256-Werte aus älteren Einstellungen blockieren Desktop-Updates nicht mehr.
Ein fehlgeschlagener App-Tools-Check erscheint als eigene Dienstdiagnose und wird
im Leerlauf regelmäßig erneut geprüft; SIP und Windows-Audio laufen weiter.
Fällt der Windows-Audioweg während eines gebundenen Voice-Anrufs aus, hält
PhoneBridge den SIP-Anruf kurz mit stummem Audio. Wenn derselbe Desktop und
Voice-Task bestätigt sind, öffnet es den Audioweg genau einmal neu. Bleibt die
Zuordnung unklar oder schlägt der Neuaufbau fehl, wird der Anruf beendet.

Über MCP zeigt `phone_bridge_status` die eingerichteten Empfänger. Mit
`phone_bridge_call` startet ein Voice-Anruf an einen dieser Empfänger;
`initialPrompt` muss Text für den Voice-Agenten enthalten. `phone_bridge_status`
liest mit `callId` den Anrufzustand, `phone_bridge_hangup` beendet ihn. Der interne
`phone.request`-Aufruf unterstützt weiterhin Windows-Audio und direktes Wählen.

Hat derselbe Empfänger bereits einen eingehenden oder ausgehenden Voice-Anruf,
sendet `phone_bridge_call` das neue `initialPrompt` unverändert an dessen aktuellen
Task und gibt die bestehende `callId` zurück. Ein laufender Verbindungsaufbau oder
Task-Wechsel wird zuerst abgeschlossen. Eine Wiederholung derselben `operationId`
sendet den Prompt nicht erneut; ihr Ergebnis ist über `phone_bridge_status` mit
dieser ID und der zurückgegebenen `callId` abrufbar.

Für Voice-Anrufe gilt zunächst **Sol / high**. Ein Folgeanruf desselben Anrufers
verwendet seinen letzten nicht archivierten PhoneBridge-Task erneut. Während des
Anrufs wählt `*1<M><R>#` das Modell (1 Luna, 2 Sol, 3 Astra) und Reasoning
(1 low, 2 medium, 3 high, 4 xhigh, 5 max, 6 ultra; Luna ohne ultra).
Die Wahl aktualisiert die folgenden Turns im aktuellen Voice-Task über Desktop
App Tools, ohne ihn neu zu starten. `*0#` erstellt einen neuen
PhoneBridge-Task mit der gewählten Kombination und überträgt die laufende
Voice-Sitzung dorthin. Nach bestätigtem Transfer sendet PhoneBridge den
konfigurierten Begrüßungsauftrag auch an den neuen Task. Für angenommene
Tastaturbefehle gibt es ein kurzes Bestätigungssignal; ein fehlgeschlagener
Wechsel erhält eine andere Tonfolge. Der bisherige Task bleibt als Verlauf
erhalten.
`phone_bridge_status` zeigt Standard und aktuelle Auswahl an.
Per MCP ändert `phone_bridge_select_voice` mit `callId`, `operationId`, `model`
(`gpt-6-luna`, `gpt-6-sol`, `gpt-6-astra`) und `reasoningEffort` dieselbe Auswahl.
`phone_bridge_restart_voice` benötigt `callId` und `operationId` und führt den
gleichen Task-Wechsel wie `*0#` aus. Der Telefonanruf bleibt verbunden.
Für jede neue Aktion eine neue `operationId` verwenden; Wiederholungen derselben
ID liefern das gespeicherte Ergebnis. Bei verlorenen Antworten liest
`phone_bridge_status` mit `callId` und `operationId` den ursprünglichen Ausgang.
Nach jedem verbundenen eingehenden Voice-Anruf wird ein Begrüßungsauftrag an den
gebundenen Task gesendet, auch wenn dieser aus einem früheren Anruf wiederverwendet
wird. `incomingInitialPrompt` in den PhoneBridge-Instanzeinstellungen bestimmt den
Wortlaut; ohne Eintrag gilt `Greet the user with "Hi"`. Direkte eingehende
Voice-Anrufe werden früh angenommen und hören während Desktop- und Task-Vorbereitung
auch bei einer vorübergehend getrennten Hive-Verbindung ein lokales Wartesignal. Die
lokale Anrufzulassung und das Journal bleiben dabei aktiv; Hive-Werkzeuge sind bis
zum Wiederverbinden nicht erreichbar.

Nach der Task-Bindung wechselt PhoneBridge auf Voice-Audio
und sendet den Begrüßungsauftrag. Ausgehende Voice-Anrufe hören nach dem Abheben
ebenfalls das Wartesignal bis zur Voice-Bindung; auf den Begrüßungsauftrag folgt
das angegebene `initialPrompt`.
