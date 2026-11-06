import { useEffect, useRef, useState, type ReactNode } from "react";
import type { SearchAddon, ISearchOptions } from "@xterm/addon-search";
import { ArrowDown, ArrowUp, CaseSensitive, Regex, WholeWord, X } from "lucide-react";

interface Props {
  search: SearchAddon;
  /** Text to start with (the terminal's selection, if any). */
  initial: string;
  onClose(): void;
}

/** Match colors drawn over the terminal (xterm wants plain hex here). */
const DECORATIONS: ISearchOptions["decorations"] = {
  matchBackground: "#d9a40055",
  matchBorder: "#d9a400",
  matchOverviewRuler: "#d9a400",
  activeMatchBackground: "#ff7a00aa",
  activeMatchBorder: "#ff7a00",
  activeMatchColorOverviewRuler: "#ff7a00",
};

/** Find in the terminal's scrollback: Enter for the next match, Shift+Enter
 *  for the previous one, Escape to close. */
export function TerminalSearch({ search, initial, onClose }: Props) {
  const [query, setQuery] = useState(initial);
  const [caseSensitive, setCaseSensitive] = useState(false);
  const [wholeWord, setWholeWord] = useState(false);
  const [regex, setRegex] = useState(false);
  const [result, setResult] = useState<{ index: number; count: number } | null>(null);
  const [badRegex, setBadRegex] = useState(false);
  const input = useRef<HTMLInputElement>(null);

  useEffect(() => {
    input.current?.focus();
    input.current?.select();
    const sub = search.onDidChangeResults(({ resultIndex, resultCount }) =>
      setResult({ index: resultIndex, count: resultCount }),
    );
    return () => {
      sub.dispose();
      search.clearDecorations();
    };
  }, [search]);

  const options = (incremental: boolean): ISearchOptions => ({
    caseSensitive,
    wholeWord,
    regex,
    incremental,
    decorations: DECORATIONS,
  });

  const find = (forward: boolean, incremental = false) => {
    if (!query) {
      search.clearDecorations();
      setResult(null);
      return;
    }
    if (regex) {
      try {
        new RegExp(query);
      } catch {
        setBadRegex(true);
        search.clearDecorations();
        setResult(null);
        return;
      }
    }
    setBadRegex(false);
    const opts = options(incremental);
    if (forward) search.findNext(query, opts);
    else search.findPrevious(query, opts);
  };

  // Search as you type, staying on the current match while it still fits.
  useEffect(() => {
    find(true, true);
  }, [query, caseSensitive, wholeWord, regex]); // eslint-disable-line react-hooks/exhaustive-deps

  const status = badRegex
    ? "Invalid pattern"
    : !query || !result
      ? ""
      : result.count === 0
        ? "No results"
        : result.index < 0
          ? `${result.count}+ matches`
          : `${result.index + 1} of ${result.count}`;

  const toggle = (on: boolean, set: (v: boolean) => void, label: string, icon: ReactNode) => (
    <button
      type="button"
      className={`icon-btn term-search-toggle ${on ? "is-toggled" : ""}`}
      aria-pressed={on}
      title={label}
      aria-label={label}
      onClick={() => set(!on)}
    >
      {icon}
    </button>
  );

  return (
    <div className="term-search" role="search" aria-label="Find in terminal">
      <input
        ref={input}
        className="term-search-input"
        value={query}
        placeholder="Find"
        aria-label="Find in terminal"
        aria-invalid={badRegex}
        spellCheck={false}
        onChange={(e) => setQuery(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            find(!e.shiftKey);
          } else if (e.key === "Escape") {
            e.preventDefault();
            onClose();
          }
        }}
      />
      {toggle(caseSensitive, setCaseSensitive, "Match case", <CaseSensitive size={15} />)}
      {toggle(wholeWord, setWholeWord, "Whole word", <WholeWord size={15} />)}
      {toggle(regex, setRegex, "Regular expression", <Regex size={15} />)}
      <span className="term-search-status" aria-live="polite">
        {status}
      </span>
      <button type="button" className="icon-btn" title="Previous (Shift+Enter)" aria-label="Previous match" onClick={() => find(false)}>
        <ArrowUp size={15} />
      </button>
      <button type="button" className="icon-btn" title="Next (Enter)" aria-label="Next match" onClick={() => find(true)}>
        <ArrowDown size={15} />
      </button>
      <button type="button" className="icon-btn" title="Close (Escape)" aria-label="Close find" onClick={onClose}>
        <X size={15} />
      </button>
    </div>
  );
}
