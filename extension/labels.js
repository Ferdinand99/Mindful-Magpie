// The interface strings of the shared renderer, in English and Norwegian. The keys are the ones
// the server writes into the start page's #mm-labels block (src/pages.rs, labels()); the texts
// are the same as in locales/en.toml and locales/nb.toml (start.empty, start.invalid_link,
// start.group_label, start.bookmarks_label, the service strings, start.block_more, the spark labels and the «Trenger deg» strings), so the new tab reads like the web page. A test keeps
// the key lists equal to the server's.

import { detail } from "./vendor/detail-labels.js";

// The titles and labels of detail blocks: the keys the server's labels() carries as `detail.<key>`
// (the `[detail]` section of the locale files), generated into vendor/detail-labels.js.
const detailLabels = (texts) => Object.fromEntries(Object.entries(texts).map(([key, text]) => [`detail.${key}`, text]));

export const labels = {
  en: {
    empty: "Your start page is empty. Edit it to add services and bookmarks.",
    invalid_link: "invalid link",
    group_label: "Group: {title}",
    bookmarks_label: "Bookmarks",
    service_missing: "This service no longer exists",
    service_waiting: "Waiting for the first answer",
    status_ok: "Status: OK",
    status_warn: "Status: warning",
    status_bad: "Status: problem",
    status_unknown: "Status: unknown",
    updated: "Last updated {when}",
    block_more: "+ {n} more",
    spark_line: "Now {value}, from {min} to {max}",
    spark_counter: "{n} in the last 24 hours",
    spark_updown: "{n} half hours with something down",
    needs: "Needs you",
    need_ack: "Acknowledge",
    need_ack_for: "Acknowledge {name}: {text}",
    need_unack: "Undo",
    need_unack_for: "Undo the acknowledgement of {name}: {text}",
    needs_acked: "{n} acknowledged",
    needs_show_acked: "Show acknowledged",
    ...detailLabels(detail.en),
  },
  nb: {
    empty: "Startsiden er tom. Rediger den for å legge til tjenester og bokmerker.",
    invalid_link: "ugyldig lenke",
    group_label: "Gruppe: {title}",
    bookmarks_label: "Bokmerker",
    service_missing: "Tjenesten finnes ikke lenger",
    service_waiting: "Venter på første svar",
    status_ok: "Status: OK",
    status_warn: "Status: advarsel",
    status_bad: "Status: feil",
    status_unknown: "Status: ukjent",
    updated: "Sist oppdatert {when}",
    block_more: "+ {n} til",
    spark_line: "Nå {value}, fra {min} til {max}",
    spark_counter: "{n} siste 24 timer",
    spark_updown: "{n} halvtimer der noe var nede",
    needs: "Trenger deg",
    need_ack: "Kvitter",
    need_ack_for: "Kvitter {name}: {text}",
    need_unack: "Angre",
    need_unack_for: "Angre kvitteringen for {name}: {text}",
    needs_acked: "{n} kvittert",
    needs_show_acked: "Vis kvitterte",
    ...detailLabels(detail.nb),
  },
};

/**
 * The language and strings for a browser language tag such as "nb-NO" or "en-GB". Norwegian
 * (nb, nn, no) gets Norwegian; every other language, and a missing tag, gets English. A key that
 * is missing from the chosen language comes from English, and from Norwegian if English lacks it
 * too, so the page never shows an empty string for a known key.
 */
export function pickLabels(language) {
  const primary = typeof language === "string" ? language.toLowerCase().split(/[-_]/)[0] : "";
  const lang = primary === "nb" || primary === "nn" || primary === "no" ? "nb" : "en";
  return { lang, labels: { ...labels.nb, ...labels.en, ...labels[lang] } };
}
