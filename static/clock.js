/* Mindful Magpie: keeps the clock and the date on the start page right.
   The server renders both; this updates them on the minute, in the site's
   time zone and language (data-tz, data-lang on #clock). Looked up on every
   tick, so it also works after the page is swapped. Never moves focus. */
(function () {
  "use strict";

  function tick() {
    var clock = document.getElementById("clock");
    if (!clock) return;
    var now = new Date();
    var zone = clock.getAttribute("data-tz") || undefined;
    var lang = clock.getAttribute("data-lang") || undefined;
    try {
      clock.textContent = new Intl.DateTimeFormat(lang, {
        timeZone: zone,
        hour: "2-digit",
        minute: "2-digit",
        hourCycle: "h23",
      }).format(now);
      var date = document.getElementById("date");
      if (date) {
        date.textContent = new Intl.DateTimeFormat(lang, {
          timeZone: zone,
          weekday: "long",
          day: "numeric",
          month: "long",
        }).format(now);
      }
    } catch (e) {
      /* An unknown zone or language: keep what the server wrote. */
    }
  }

  /* Next tick on the minute (a timer drifts, and a hidden tab pauses it). */
  function schedule() {
    tick();
    setTimeout(schedule, 60000 - (Date.now() % 60000) + 50);
  }

  document.addEventListener("visibilitychange", function () {
    if (!document.hidden) tick();
  });
  schedule();
})();
