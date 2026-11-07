// The interface strings of the shared renderer, in English and Norwegian. The keys are the ones
// the server writes into the start page's #mm-labels block (src/pages.rs, labels()); the texts
// are the same as in locales/en.toml and locales/nb.toml (start.empty, start.invalid_link,
// start.group_label, start.bookmarks_label), so the new tab reads like the web page. A test keeps
// the key lists equal to the server's.

export const labels = {
  en: {
    empty: "Your start page is empty. Edit it to add services and bookmarks.",
    invalid_link: "invalid link",
    group_label: "Group: {title}",
    bookmarks_label: "Bookmarks",
  },
  nb: {
    empty: "Startsiden er tom. Rediger den for å legge til tjenester og bokmerker.",
    invalid_link: "ugyldig lenke",
    group_label: "Gruppe: {title}",
    bookmarks_label: "Bokmerker",
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
