# Localization Quality Reference

Load this reference only when a Foxy locale task needs quality guidance beyond the
main workflow.

## Practical Rules

- Add or inspect context for short labels, commands, and strings with placeholders.
  Microsoft's globalization guidance calls out UI location, usage, conditions, and
  placeholder examples as useful translator context:
  https://learn.microsoft.com/en-us/globalization/internationalization/contextual-metadata
- Prefer complete localizable strings over fragments. Microsoft warns that composing
  sentences from string fragments and variables can break word order, punctuation,
  articles, and grammar in other languages:
  https://learn.microsoft.com/en-us/globalization/internationalization/message-formatting
- Preserve named placeholders exactly, but allow their position to change. Named
  placeholders are clearer to translators and support more natural word order:
  https://learn.microsoft.com/en-us/globalization/internationalization/message-formatting
- Use UTF-8 and validate content. W3C internationalization quick tips recommend
  Unicode/UTF-8 and validation for multilingual content:
  https://www.w3.org/International/quicktips/index
- Keep variables and trademarks stable. Mozilla's localization style guide says
  variables should not be translated and trademarks should stay in their original
  wording:
  https://mozilla-l10n.github.io/styleguides/mozilla_general/

## Foxy-Specific Review Checklist

- The translation is natural target-language UI, not a literal English word order.
- Placeholder sets match exactly between English and the target value.
- Brand and technical terms remain stable: Foxy, Swifty, Arma 3, Steam, GitHub,
  BLAKE3, MD5, TeamSpeak 3, TS3.
- Destructive labels remain visibly destructive in meaning.
- Newline-bearing confirmation strings keep their line-break intent.
- The checker passes with `--strict` and with the changed-key file.
