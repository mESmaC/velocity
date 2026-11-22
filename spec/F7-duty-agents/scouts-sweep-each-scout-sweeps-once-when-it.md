#### Scouts · Sweep · Each scout sweeps once when it joins, then on this cadence

<!-- split-spec: {"order":1104,"kind":"markdown","indentation":2} -->
- Each scout sweeps once when it joins, then on this cadence.
  "30m minimum" sweeps when the base tip differs from the last swept tip and at least 30 minutes have passed since that sweep started.
  "After every land" sweeps as soon as the changed tip is observed.
  When starting or adding scouts, you can opt those scouts into a sweep with fresh generated inputs every 30 minutes on an unchanged base in either recurring mode.
  This setting is saved per scout and defaults to off.
  A user instruction can start a sweep regardless of the cadence or base tip.
  Pausing lets running sweeps finish and starts none.
  On restart the saved tip and start time still govern scheduled sweeps.
