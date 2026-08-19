import {
  Fragment,
  type SubmitEvent,
  type ReactNode,
  useDeferredValue,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { createPortal } from "react-dom";
import {
  MarkdownDocument,
  type MarkdownImage,
} from "./components/MarkdownDocument.tsx";
import { LibraryGraph } from "./components/LibraryGraph.tsx";
import {
  ZenWorkspace,
  type ZenMentionTarget,
} from "./components/ZenWorkspace.tsx";
import type { PersistedZenDocument } from "./data/db";
import type { GraphMentionSource } from "./data/graph.ts";
import {
  type LibraryState,
  type CapturedDocumentReference,
  type PendingSyncChange,
  type UndoableChange,
  type ZenDocumentView,
  libraryRepository,
} from "./data/library.ts";
import {
  browserSync,
  type BrowserSyncState,
} from "./data/sync.ts";
import {
  createProfile,
  deleteProfile,
  listProfiles,
  renameProfile,
  type LibraryProfile,
} from "./data/profiles.ts";

/** Sentinel value for the create action inside the library switcher. */
const NEW_LIBRARY_OPTION = "__new-library__";

/** Tags shown in the rail before the rest move behind the manage action. */
const RAIL_TAG_LIMIT = 8;

type AddInput = Parameters<typeof libraryRepository.add>[0];
type EditInput = Parameters<typeof libraryRepository.edit>[1];

interface LibraryItemView {
  id: string;
  url: string;
  title?: string | null;
  excerpt?: string | null;
  note?: string | null;
  tags: string[];
  favorite: boolean;
  deleted: boolean;
  savedAt: string;
  capturedDocument?: CapturedDocumentReference | null;
}

type LifecycleFilter = "active" | "deleted" | "all";
type SearchScope = "all" | "title" | "url" | "context" | "tags";
type SortMode = "recent" | "oldest" | "title";
type TagMatchMode = "any" | "all";
type WorkspaceView = "library" | "settings" | "sync" | "zen";
/**
 * Graph is a projection of the library, not a fourth view: the same search,
 * tags, favourites and lifecycle filter drive both, so switching modes never
 * changes the result set.
 */
type LibraryMode = "list" | "graph";
type Density = "comfortable" | "compact";

interface ThemeColors {
  text: string;
  background: string;
  primary: string;
  secondary: string;
  accent: string;
}

interface ThemePreset {
  id: string;
  label: string;
  colors: ThemeColors;
}

interface UndoNotice {
  change: UndoableChange;
  message: string;
}

const DENSITY_STORAGE_KEY = "researchpocket.ui.density";
const LIBRARY_MODE_STORAGE_KEY = "researchpocket.ui.library-mode";
const RAIL_COLLAPSED_STORAGE_KEY = "researchpocket.ui.rail-collapsed";
const THEME_STORAGE_KEY = "researchpocket.ui.theme";
const IMAGE_BACKGROUND_STORAGE_KEY = "researchpocket.ui.image-background";
const READER_MEASURE_STORAGE_KEY = "researchpocket.ui.reader-measure";
const DEFAULT_IMAGE_BACKGROUND = "#ffffff";

/**
 * How wide the reader lets a line of text run. The values are the text column
 * only — the stylesheet adds the gutters back — so they read as a measure and
 * not as a pane width. `100%` is the one that opts out: it always resolves
 * wider than the column, so the cap never binds.
 */
const READER_MEASURES = [
  { id: "narrow", label: "Narrow", measure: "34rem" },
  { id: "medium", label: "Medium", measure: "42rem" },
  { id: "wide", label: "Wide", measure: "52rem" },
  { id: "full", label: "Full width", measure: "100%" },
] as const;

type ReaderMeasure = (typeof READER_MEASURES)[number]["id"];

const DEFAULT_READER_MEASURE: ReaderMeasure = "medium";

const DEFAULT_THEME: ThemeColors = {
  text: "#f5f1e9",
  background: "#1a150c",
  primary: "#d5c8a4",
  secondary: "#33656f",
  accent: "#5156ae",
};

const THEME_PRESETS: ThemePreset[] = [
  { id: "researchpocket", label: "ResearchPocket", colors: DEFAULT_THEME },
  {
    id: "dracula",
    label: "Dracula",
    colors: {
      text: "#f8f8f2",
      background: "#282a36",
      primary: "#bd93f9",
      secondary: "#8be9fd",
      accent: "#ff79c6",
    },
  },
  {
    id: "nord",
    label: "Nord",
    colors: {
      text: "#eceff4",
      background: "#2e3440",
      primary: "#88c0d0",
      secondary: "#81a1c1",
      accent: "#b48ead",
    },
  },
  {
    id: "solarized-dark",
    label: "Solarized Dark",
    colors: {
      text: "#eee8d5",
      background: "#002b36",
      primary: "#b58900",
      secondary: "#2aa198",
      accent: "#268bd2",
    },
  },
  {
    id: "gruvbox-dark",
    label: "Gruvbox Dark",
    colors: {
      text: "#ebdbb2",
      background: "#282828",
      primary: "#fabd2f",
      secondary: "#8ec07c",
      accent: "#d3869b",
    },
  },
  {
    id: "catppuccin-mocha",
    label: "Catppuccin Mocha",
    colors: {
      text: "#cdd6f4",
      background: "#1e1e2e",
      primary: "#cba6f7",
      secondary: "#89b4fa",
      accent: "#f5c2e7",
    },
  },
];

const THEME_FIELDS: { key: keyof ThemeColors; label: string }[] = [
  { key: "text", label: "Text" },
  { key: "background", label: "Background" },
  { key: "primary", label: "Primary" },
  { key: "secondary", label: "Secondary" },
  { key: "accent", label: "Accent" },
];

const LIST_BATCH_SIZE = 100;

const EMPTY_LIBRARY_STATE: LibraryState = {
  error: null,
  initialized: false,
  items: [],
  loading: true,
  pendingChanges: [],
  pendingCount: 0,
  status: "opening",
};

const EMPTY_SYNC_STATE: BrowserSyncState = {
  configuration: null,
  credentialAvailable: false,
  syncing: false,
  status: "Private sync is not connected",
  error: null,
  lastCycle: null,
};

export function App() {
  const [libraryState, setLibraryState] =
    useState<LibraryState>(EMPTY_LIBRARY_STATE);
  const [syncState, setSyncState] = useState<BrowserSyncState>(EMPTY_SYNC_STATE);
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<LifecycleFilter>("active");
  const [searchScope, setSearchScope] = useState<SearchScope>("all");
  const [favoriteOnly, setFavoriteOnly] = useState(false);
  const [selectedTags, setSelectedTags] = useState<string[]>([]);
  const [tagSearch, setTagSearch] = useState("");
  const [tagMatchMode, setTagMatchMode] = useState<TagMatchMode>("all");
  const [sortMode, setSortMode] = useState<SortMode>("recent");
  const [density, setDensity] = useState<Density>(() => readDensityPreference());
  const [theme, setTheme] = useState<ThemeColors>(() => readThemePreference());
  const [imageBackground, setImageBackground] = useState(
    () => readImageBackgroundPreference(),
  );
  const [readerMeasure, setReaderMeasure] = useState<ReaderMeasure>(() =>
    readReaderMeasurePreference(),
  );
  const [filtersOpen, setFiltersOpen] = useState(false);
  const [visibleLimit, setVisibleLimit] = useState(LIST_BATCH_SIZE);
  const [view, setView] = useState<WorkspaceView>(() => readWorkspaceView());
  const [libraryMode, setLibraryMode] = useState<LibraryMode>(() =>
    readInitialLibraryMode(),
  );
  const [railCollapsed, setRailCollapsed] = useState(() =>
    readRailCollapsedPreference(),
  );
  const [zenMentions, setZenMentions] = useState<GraphMentionSource[]>([]);
  const [capturing, setCapturing] = useState(false);
  const [commandOpen, setCommandOpen] = useState(false);
  const [readerItem, setReaderItem] = useState<LibraryItemView | null>(null);
  const [editingItem, setEditingItem] = useState<LibraryItemView | null>(null);
  const [busyAction, setBusyAction] = useState<string | null>(null);
  const [autoBootstrapAttempted, setAutoBootstrapAttempted] = useState(false);
  const [announcement, setAnnouncement] = useState("");
  const [localError, setLocalError] = useState<string | null>(null);
  const [undoNotice, setUndoNotice] = useState<UndoNotice | null>(null);
  const [zenDocuments, setZenDocuments] = useState<PersistedZenDocument[]>([]);
  const [openZen, setOpenZen] = useState<{
    documentId: string;
    view: ZenDocumentView;
  } | null>(null);
  const [profiles, setProfiles] = useState<LibraryProfile[]>([]);
  const [activeProfileId, setActiveProfileId] = useState<string | null>(null);
  // Read only by the history listener, which needs the current document
  // without re-subscribing every time one is opened.
  const openZenId = useRef<string | null>(null);
  openZenId.current = openZen?.documentId ?? null;
  const captureOpenerRef = useRef<HTMLButtonElement | null>(null);
  const editOpenerRef = useRef<HTMLButtonElement | null>(null);

  useEffect(() => {
    let active = true;
    const unsubscribe = libraryRepository.subscribe((nextState) => {
      if (active) {
        setLibraryState(nextState);
      }
    });
    const unsubscribeSync = browserSync.subscribe((nextState) => {
      if (active) setSyncState(nextState);
    });
    const unsubscribeProfile = libraryRepository.subscribeProfile((profile) => {
      if (!active) return;
      setActiveProfileId(profile.id);
      void listProfiles().then((next) => {
        if (active) setProfiles(next);
      });
    });

    return () => {
      active = false;
      unsubscribe();
      unsubscribeSync();
      unsubscribeProfile();
    };
  }, []);

  // Loaded for every view, not just Zen: the command palette and the mobile
  // navigation both surface documents from anywhere. The index is metadata
  // only, so this stays cheap no matter how large the documents are.
  useEffect(() => {
    if (!libraryState.initialized) return;
    void libraryRepository
      .listZenDocuments()
      .then(setZenDocuments)
      .catch(() => setZenDocuments([]));
  }, [view, libraryState.initialized]);

  useEffect(() => {
    try {
      window.localStorage.setItem(DENSITY_STORAGE_KEY, density);
    } catch {
      // The preference remains active for this tab when storage is unavailable.
    }
  }, [density]);

  useEffect(() => {
    try {
      window.localStorage.setItem(
        RAIL_COLLAPSED_STORAGE_KEY,
        String(railCollapsed),
      );
    } catch {
      // The preference remains active for this tab when storage is unavailable.
    }
  }, [railCollapsed]);

  // The remembered mode is written into the URL once, on open. After that the
  // parameter is the only thing that decides, so going back out of the graph
  // returns to the list instead of being overruled by the preference.
  useEffect(() => {
    const params = new URLSearchParams(window.location.search);
    if ((params.get("mode") === "graph" ? "graph" : "list") === libraryMode) return;
    if (libraryMode === "graph") params.set("mode", "graph");
    else params.delete("mode");
    window.history.replaceState(
      window.history.state,
      "",
      formatUrl(params, window.location.hash),
    );
    // Runs for the opening preference only; every later change writes its own
    // history entry through changeLibraryMode.
  }, []);

  // Co-mention edges are the one part of the graph that is not already in
  // memory, so the bodies are read only when the graph is actually on screen.
  useEffect(() => {
    if (!libraryState.initialized) return;
    if (view !== "library" || libraryMode !== "graph") return;
    let active = true;
    void libraryRepository
      .listZenMentions()
      .then((next) => {
        if (active) setZenMentions(next);
      })
      .catch(() => {
        if (active) setZenMentions([]);
      });
    return () => {
      active = false;
    };
  }, [libraryMode, libraryState.initialized, view, zenDocuments]);

  useEffect(() => {
    applyThemePreference(theme);
    try {
      if (themesEqual(theme, DEFAULT_THEME)) {
        window.localStorage.removeItem(THEME_STORAGE_KEY);
      } else {
        window.localStorage.setItem(
          THEME_STORAGE_KEY,
          JSON.stringify({ colors: theme, version: 1 }),
        );
      }
    } catch {
      // The preference remains active for this tab when storage is unavailable.
    }
  }, [theme]);

  useEffect(() => {
    document.documentElement.style.setProperty(
      "--reader-image-background",
      imageBackground,
    );
    try {
      if (imageBackground === DEFAULT_IMAGE_BACKGROUND) {
        window.localStorage.removeItem(IMAGE_BACKGROUND_STORAGE_KEY);
      } else {
        window.localStorage.setItem(
          IMAGE_BACKGROUND_STORAGE_KEY,
          imageBackground,
        );
      }
    } catch {
      // The preference remains active for this tab when storage is unavailable.
    }
  }, [imageBackground]);

  useEffect(() => {
    const choice = READER_MEASURES.find(({ id }) => id === readerMeasure);
    const root = document.documentElement;
    // The default lives in tokens.css, so choosing it clears the override
    // rather than restating the value in two places.
    if (choice && readerMeasure !== DEFAULT_READER_MEASURE) {
      root.style.setProperty("--reader-measure", choice.measure);
    } else {
      root.style.removeProperty("--reader-measure");
    }
    try {
      if (readerMeasure === DEFAULT_READER_MEASURE) {
        window.localStorage.removeItem(READER_MEASURE_STORAGE_KEY);
      } else {
        window.localStorage.setItem(READER_MEASURE_STORAGE_KEY, readerMeasure);
      }
    } catch {
      // The preference remains active for this tab when storage is unavailable.
    }
  }, [readerMeasure]);

  useEffect(() => {
    function handleShortcut(event: KeyboardEvent) {
      const target = event.target as HTMLElement | null;
      const isTyping =
        target?.matches("input, textarea, select") || target?.isContentEditable;

      if (
        event.ctrlKey &&
        event.shiftKey &&
        !event.altKey &&
        !event.metaKey &&
        event.key.toLowerCase() === "p"
      ) {
        event.preventDefault();
        setCommandOpen(true);
        return;
      }

      if (event.key === "Escape") {
        setCommandOpen(false);
        if (filtersOpen) {
          setFiltersOpen(false);
        } else {
          closeReader();
        }
        return;
      }

      if (
        !isTyping &&
        (event.ctrlKey || event.metaKey) &&
        !event.altKey &&
        !event.shiftKey &&
        event.key.toLowerCase() === "z" &&
        undoNotice &&
        busyAction === null
      ) {
        event.preventDefault();
        void undoLastChange();
        return;
      }

      if (!isTyping && event.key === "/") {
        event.preventDefault();
        document.querySelector<HTMLInputElement>("#library-search")?.focus();
      }
    }

    window.addEventListener("keydown", handleShortcut);
    return () => window.removeEventListener("keydown", handleShortcut);
  }, [busyAction, filtersOpen, undoNotice, view]);

  // Pasting a URL with nothing focused stages it in the omnibar and puts the
  // caret there. It stops short of saving: writing an item straight from the
  // clipboard is not a thing to do without the owner seeing what it was.
  useEffect(() => {
    function handlePaste(event: ClipboardEvent) {
      if (view !== "library") return;
      if (readerItem || capturing || editingItem || commandOpen) return;
      const target = event.target as HTMLElement | null;
      if (target?.matches("input, textarea, select") || target?.isContentEditable) {
        return;
      }
      const pasted = event.clipboardData?.getData("text")?.trim() ?? "";
      if (!readOmnibarUrl(pasted)) return;
      event.preventDefault();
      setQuery(pasted);
      document.querySelector<HTMLInputElement>("#library-search")?.focus();
    }

    window.addEventListener("paste", handlePaste);
    return () => window.removeEventListener("paste", handlePaste);
  }, [capturing, commandOpen, editingItem, readerItem, view]);

  const items = libraryState.items as unknown as LibraryItemView[];
  const activeCount = items.filter((item) => !item.deleted).length;
  const deletedCount = items.length - activeCount;
  const knownTags = useMemo(
    () =>
      Array.from(new Set(items.flatMap((item) => item.tags))).sort((left, right) =>
        left.localeCompare(right),
      ),
    [items],
  );
  const tagCounts = useMemo(
    () =>
      knownTags.map((tag) => ({
        count: items.filter((item) => !item.deleted && item.tags.includes(tag)).length,
        tag,
      })),
    [items, knownTags],
  );

  // The rail shows the busiest tags plus anything already filtered on; the
  // rest stay behind the manage action so the rail cannot grow unbounded.
  const railTags = useMemo(() => {
    const ranked = [...tagCounts].sort(
      (left, right) => right.count - left.count || left.tag.localeCompare(right.tag),
    );
    const shown = ranked.slice(0, RAIL_TAG_LIMIT);
    const shownTags = new Set(shown.map(({ tag }) => tag));
    const pinned = ranked.filter(
      ({ tag }) => selectedTags.includes(tag) && !shownTags.has(tag),
    );
    return [...shown, ...pinned];
  }, [selectedTags, tagCounts]);
  const hiddenTagCount = tagCounts.length - railTags.length;

  useEffect(() => {
    function restoreNavigationFromHistory() {
      // The projection rides the URL, so back out of the graph lands on the
      // list whichever surface the entry above it was.
      setLibraryMode(readLibraryModeFromUrl());

      const item = window.location.hash.match(/^#item=(.+)$/);
      if (item) {
        const itemId = decodeURIComponent(item[1]!);
        setReaderItem(items.find((entry) => entry.id === itemId) ?? null);
        return;
      }
      setReaderItem(null);
      setView(readWorkspaceView());

      const document = window.location.hash.match(/^#document=(.+)$/);
      if (!document) {
        setOpenZen(null);
        return;
      }
      const documentId = decodeURIComponent(document[1]!);
      // Already loaded when this runs for an entry we pushed ourselves; only a
      // real back, forward, or deep link has to fetch the document.
      if (documentId === openZenId.current) return;
      if (libraryState.initialized) void loadZenDocument(documentId);
    }

    window.addEventListener("popstate", restoreNavigationFromHistory);
    restoreNavigationFromHistory();
    return () =>
      window.removeEventListener("popstate", restoreNavigationFromHistory);
  }, [items, libraryState.initialized]);
  const deferredQuery = useDeferredValue(query);
  const visibleItems = useMemo(
    () =>
      filterAndSortItems(
        items,
        deferredQuery,
        filter,
        searchScope,
        favoriteOnly,
        selectedTags,
        tagMatchMode,
        sortMode,
      ),
    [
      deferredQuery,
      favoriteOnly,
      filter,
      items,
      searchScope,
      selectedTags,
      sortMode,
      tagMatchMode,
    ],
  );
  const searchPending = query !== deferredQuery;
  const renderedItems = visibleItems.slice(0, visibleLimit);
  const tagFilterMatches = useMemo(() => {
    const normalizedSearch = tagSearch.trim().toLocaleLowerCase();
    return knownTags
      .filter((tag) => !selectedTags.includes(tag))
      .filter(
        (tag) =>
          !normalizedSearch || tag.toLocaleLowerCase().includes(normalizedSearch),
      )
      .slice(0, normalizedSearch ? 20 : 8);
  }, [knownTags, selectedTags, tagSearch]);
  const appliedFilterCount =
    Number(filter !== "active") +
    Number(searchScope !== "all") +
    Number(favoriteOnly) +
    Number(selectedTags.length > 0) +
    Number(sortMode !== "recent");

  useEffect(() => {
    setVisibleLimit(LIST_BATCH_SIZE);
  }, [deferredQuery, favoriteOnly, filter, searchScope, selectedTags, sortMode, tagMatchMode]);

  async function runAction(
    action: string,
    successMessage: string,
    operation: () => Promise<unknown>,
  ) {
    setBusyAction(action);
    setLocalError(null);
    try {
      await operation();
      setAnnouncement(successMessage);
      return true;
    } catch (error: unknown) {
      setLocalError(readError(error));
      setAnnouncement(`${action} failed.`);
      return false;
    } finally {
      setBusyAction(null);
    }
  }

  async function initializeLibrary(targetView: WorkspaceView) {
    const initialized = await runAction(
      "Initialize library",
      "Your private local library is ready.",
      () => libraryRepository.initialize(),
    );
    if (initialized) {
      navigateToView(targetView);
    }
  }

  function navigateToView(nextView: WorkspaceView) {
    const nextUrl = `${window.location.pathname}${window.location.search}${hashForView(nextView)}`;
    const currentUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
    if (nextUrl !== currentUrl) {
      window.history.pushState(window.history.state, "", nextUrl);
    }
    setReaderItem(null);
    setView(nextView);
  }

  /**
   * Switching projection is a navigation the way opening a document is: the
   * phone's only back gesture has to return to the list rather than leave the
   * application, so the change earns its own history entry.
   */
  function changeLibraryMode(nextMode: LibraryMode) {
    if (nextMode === libraryMode) return;
    setLibraryMode(nextMode);
    try {
      window.localStorage.setItem(LIBRARY_MODE_STORAGE_KEY, nextMode);
    } catch {
      // The preference remains active for this tab when storage is unavailable.
    }
    const params = new URLSearchParams(window.location.search);
    if (nextMode === "graph") params.set("mode", "graph");
    else params.delete("mode");
    const nextUrl = formatUrl(params, window.location.hash);
    if (nextUrl !== `${window.location.pathname}${window.location.search}${window.location.hash}`) {
      window.history.pushState(window.history.state, "", nextUrl);
    }
  }

  async function refreshZenDocuments() {
    setZenDocuments(await libraryRepository.listZenDocuments());
  }

  /** Loads a document into the workspace without touching history. */
  async function loadZenDocument(documentId: string) {
    try {
      setOpenZen({
        documentId,
        view: await libraryRepository.zenDocument(documentId),
      });
      return true;
    } catch (error) {
      setLocalError(error instanceof Error ? error.message : "That document could not be opened.");
      return false;
    }
  }

  /**
   * Opening a document is a navigation, so it earns a history entry. Without
   * one the browser's back gesture leaves the application entirely instead of
   * returning to the index, which is the only back a phone has.
   */
  async function openZenDocument(documentId: string) {
    if (!(await loadZenDocument(documentId))) return;
    const nextUrl = `${window.location.pathname}${window.location.search}#document=${encodeURIComponent(documentId)}`;
    if (nextUrl === `${window.location.pathname}${window.location.search}${window.location.hash}`) {
      return;
    }
    window.history.pushState(
      { ...window.history.state, researchPocketZen: true },
      "",
      nextUrl,
    );
  }

  function closeZenDocument() {
    setOpenZen(null);
    if (!window.location.hash.startsWith("#document=")) return;
    // Unwinding our own entry keeps forward working and stops the stack from
    // growing every time a document is opened and closed.
    if (window.history.state?.researchPocketZen) {
      window.history.back();
      return;
    }
    window.history.replaceState(
      window.history.state,
      "",
      `${window.location.pathname}${window.location.search}${hashForView("zen")}`,
    );
  }

  async function createZenDocument(title: string) {
    await runZen(async () => {
      const documentId = await libraryRepository.createZenDocument({
        title: title.trim() || null,
        body: "",
        tags: [],
      });
      await refreshZenDocuments();
      await openZenDocument(documentId);
    });
  }

  async function saveZenBody(documentId: string, body: string) {
    await runZen(async () => {
      await libraryRepository.setZenBody(documentId, body);
      await refreshZenDocuments();
      await loadZenDocument(documentId);
    });
  }

  async function saveZenTitle(documentId: string, title: string | null) {
    await runZen(async () => {
      await libraryRepository.setZenTitle(documentId, title);
      await refreshZenDocuments();
      await loadZenDocument(documentId);
    });
  }

  async function removeZenDocument(documentId: string) {
    await runZen(async () => {
      await libraryRepository.deleteZenDocument(documentId);
      closeZenDocument();
      await refreshZenDocuments();
    });
  }

  /** Shared busy/error handling so every zen action reports the same way. */
  async function runZen(action: () => Promise<void>) {
    setBusyAction("zen");
    setLocalError(null);
    try {
      await action();
    } catch (error) {
      setLocalError(
        error instanceof Error ? error.message : "That change was not saved.",
      );
    } finally {
      setBusyAction(null);
    }
  }

  /** Mentions resolve against the loaded library, so deletes stay visible. */
  function resolveZenMention(itemId: string): ZenMentionTarget | null {
    const item = items.find((candidate) => candidate.id === itemId);
    if (!item) return null;
    return { title: item.title ?? null, url: item.url, deleted: item.deleted };
  }

  async function refreshProfiles() {
    setProfiles(await listProfiles());
  }

  async function switchLibrary(profileId: string) {
    if (profileId === activeProfileId) return;
    setLocalError(null);
    try {
      const profile = await libraryRepository.switchProfile(profileId);
      await refreshProfiles();
      setAutoBootstrapAttempted(false);
      navigateToView("library");
      setAnnouncement(`Opened ${profile.name}`);
    } catch (error) {
      setLocalError(readError(error));
    }
  }

  async function createLibrary() {
    const name = window.prompt("Name the new library");
    if (name === null) return;
    setLocalError(null);
    try {
      const created = await createProfile(name);
      await refreshProfiles();
      await switchLibrary(created.id);
    } catch (error) {
      setLocalError(readError(error));
    }
  }

  useEffect(() => {
    if (
      libraryState.loading ||
      libraryState.initialized ||
      autoBootstrapAttempted ||
      busyAction !== null
    ) {
      return;
    }

    const target = window.location.hash;
    if (target !== "#new" && target !== "#restore") return;
    setAutoBootstrapAttempted(true);
    void initializeLibrary(target === "#restore" ? "sync" : "library");
  }, [autoBootstrapAttempted, busyAction, libraryState.initialized, libraryState.loading]);

  async function addItem(input: AddInput) {
    let undo: UndoableChange | null = null;
    const saved = await runAction("Save link", "Saved to your local library.", () =>
      libraryRepository.add(input).then((change) => {
        undo = change;
      }),
    );
    if (saved) {
      offerUndo(undo, "Saved link.");
      navigateToView("library");
      closeCapture();
    }
    return saved;
  }

  async function deleteItem(item: LibraryItemView) {
    let undo: UndoableChange | null = null;
    const deleted = await runAction("Delete link", "Moved to deleted items.", () =>
      libraryRepository.remove(item.id).then((change) => {
        undo = change;
      }),
    );
    if (deleted) offerUndo(undo, "Moved link to deleted items.");
  }

  async function restoreItem(item: LibraryItemView) {
    let undo: UndoableChange | null = null;
    const restored = await runAction("Restore link", "Link restored.", () =>
      libraryRepository.restore(item.id).then((change) => {
        undo = change;
      }),
    );
    if (restored) {
      offerUndo(undo, "Restored link.");
      setFilter("active");
    }
  }

  async function toggleFavorite(item: LibraryItemView) {
    let undo: UndoableChange | null = null;
    const saved = await runAction(
      item.favorite ? "Remove favorite" : "Add favorite",
      item.favorite ? "Removed from favorites." : "Added to favorites.",
      () =>
        libraryRepository.edit(item.id, {
          favorite: !item.favorite,
          expectedNote: item.note ?? null,
        }).then((change) => {
          undo = change;
        }),
    );
    if (saved) {
      offerUndo(
        undo,
        item.favorite ? "Removed from favorites." : "Added to favorites.",
      );
    }
  }

  async function saveEdit(item: LibraryItemView, input: EditInput) {
    let undo: UndoableChange | null = null;
    const saved = await runAction("Update link", "Changes saved locally.", () =>
      libraryRepository.edit(item.id, {
        ...input,
        expectedNote: item.note ?? null,
      }).then((change) => {
        undo = change;
      }),
    );
    if (saved) {
      offerUndo(undo, "Changes saved.");
      closeEditor();
    }
    return saved;
  }

  function offerUndo(change: UndoableChange | null, message: string) {
    if (!change) return;
    setUndoNotice({ change, message });
    setAnnouncement(`${message} Undo is available.`);
  }

  async function undoLastChange() {
    const notice = undoNotice;
    if (!notice || busyAction !== null) return;
    const undone = await runAction("Undo change", "Last change undone.", () =>
      libraryRepository.undo(notice.change),
    );
    if (undone) {
      setUndoNotice(null);
      setAnnouncement("Last change undone.");
    }
  }

  function openEditor(item: LibraryItemView, opener: HTMLButtonElement) {
    setLocalError(null);
    editOpenerRef.current = opener;
    setEditingItem(item);
  }

  function openReader(item: LibraryItemView) {
    const nextUrl = `${window.location.pathname}${window.location.search}#item=${encodeURIComponent(item.id)}`;
    const nextState = { ...window.history.state, researchPocketReader: true };
    window.history.pushState(nextState, "", nextUrl);
    setView("library");
    setReaderItem(item);
  }

  function closeReader() {
    if (!window.location.hash.startsWith("#item=")) {
      setReaderItem(null);
      return;
    }
    if (window.history.state?.researchPocketReader) {
      window.history.back();
      return;
    }
    window.history.replaceState(
      window.history.state,
      "",
      `${window.location.pathname}${window.location.search}${hashForView(view)}`,
    );
    setReaderItem(null);
  }

  function toggleTagFilter(tag: string) {
    const selected = selectedTags.includes(tag);
    setSelectedTags((current) =>
      current.includes(tag)
        ? current.filter((value) => value !== tag)
        : [...current, tag],
    );
    setTagSearch("");
    setFiltersOpen(true);
    navigateToView("library");
    setAnnouncement(
      selected ? `Removed ${tag} tag filter.` : `Filtering by ${tag}.`,
    );
  }

  function openCapture(opener: HTMLButtonElement) {
    setLocalError(null);
    captureOpenerRef.current = opener;
    setCapturing(true);
  }

  function closeCapture() {
    setCapturing(false);
    setLocalError(null);
    window.requestAnimationFrame(() => {
      const opener = captureOpenerRef.current;
      if (opener?.isConnected) opener.focus();
      captureOpenerRef.current = null;
    });
  }

  function closeEditor() {
    setEditingItem(null);
    setLocalError(null);
    window.requestAnimationFrame(() => {
      const opener = editOpenerRef.current;
      if (opener?.isConnected) opener.focus();
      editOpenerRef.current = null;
    });
  }

  const repositoryError = readStateError(libraryState.error);
  const displayedError = localError ?? repositoryError;

  // A Zen document remains a focused writing surface. Saved items, Sync, and
  // Settings all use the normal workspace shell so navigation does not change
  // shape as the owner moves through the application.
  const pushedScreen = openZen
    ? { label: "Zen", onBack: closeZenDocument }
    : null;
  const railApplies = !openZen;

  if (libraryState.loading) {
    return <BootScreen />;
  }

  if (!libraryState.initialized) {
    return (
      <Welcome
        busy={busyAction !== null}
        error={displayedError}
        onInitialize={() => initializeLibrary("library")}
        onRestore={() => initializeLibrary("sync")}
        restoreFirst={window.location.hash === "#restore"}
      />
    );
  }

  return (
    <div className="app-shell">
      <a className="skip-link" href="#workspace">
        Skip to workspace
      </a>

      <div className="workspace-chrome">
        <header className={`masthead${pushedScreen ? " masthead-pushed" : ""}`}>
          <div className="masthead-lead">
            {pushedScreen ? (
              <button
                className="masthead-back"
                onClick={pushedScreen.onBack}
                type="button"
              >
                <span aria-hidden="true">←</span>
                <span>{pushedScreen.label}</span>
              </button>
            ) : null}
            {/* The sidebar control leads the brand as part of one compact
                header cluster; it is not positioned against the rail seam. */}
            {railApplies ? (
              <button
                aria-controls="workspace-sidebar"
                aria-expanded={!railCollapsed}
                aria-label={railCollapsed ? "Show sidebar" : "Hide sidebar"}
                className="workspace-rail-toggle"
                onClick={() => setRailCollapsed((collapsed) => !collapsed)}
                title={railCollapsed ? "Show sidebar" : "Hide sidebar"}
                type="button"
              >
                <svg aria-hidden="true" viewBox="0 0 20 20">
                  <path d="M3 5.5h14M3 10h14M3 14.5h14" />
                </svg>
              </button>
            ) : null}
            <button
              aria-label={view === "library" ? "ResearchPocket library" : "Back to library"}
              className="brand-lockup"
              onClick={() => navigateToView("library")}
              type="button"
            >
              <span aria-hidden="true" className="brand-mark">
                rp
              </span>
              <p className="brand-name">ResearchPocket</p>
              {/* A phone has no room for the product name and every reason to
                  say which screen it is on, so the two swap places there. */}
              <p className="brand-view">{formatViewLabel(view)}</p>
            </button>
          </div>

          <div className="masthead-actions">
            {profiles.length > 1 ? (
              <div className="library-switcher">
                <select
                  aria-label="Active library"
                  onChange={(event) => {
                    if (event.target.value === NEW_LIBRARY_OPTION) {
                      event.target.value = activeProfileId ?? "";
                      void createLibrary();
                      return;
                    }
                    void switchLibrary(event.target.value);
                  }}
                  value={activeProfileId ?? ""}
                >
                  {profiles.map((profile) => (
                    <option key={profile.id} value={profile.id}>
                      {profile.name}
                    </option>
                  ))}
                  <option value={NEW_LIBRARY_OPTION}>＋ New library…</option>
                </select>
              </div>
            ) : null}
            {/* The one route to Sync that is on screen from every view, so it
                carries a chip's outline and says what pressing it does. */}
            <button
              aria-label={`Open sync — ${formatHeaderStatus(libraryState.status, libraryState.pendingCount)}`}
              className="local-status"
              data-pending={libraryState.pendingCount > 0 ? "true" : undefined}
              onClick={() => navigateToView("sync")}
              title="Open sync"
              type="button"
            >
              <span aria-hidden="true" className="status-dot" />
              <span className="status-label">local</span>
              <span aria-live="polite" className="status-copy" role="status">
                {formatSyncCopy(libraryState.status, libraryState.pendingCount)}
              </span>
              {/* The full status sentence does not fit on a phone, so a shorter
                  reading of the same state stands in for it there. */}
              <span aria-hidden="true" className="status-chip">
                {formatSyncChip(libraryState.status, libraryState.pendingCount)}
              </span>
            </button>
            <button
              className="command-trigger"
              onClick={() => setCommandOpen(true)}
              type="button"
            >
              search <kbd>Ctrl Shift P</kbd>
            </button>
          </div>
        </header>
      </div>

      <div
        className={`workspace-layout${
          !railApplies
            ? " workspace-layout-rail-hidden"
            : railCollapsed
              ? " workspace-layout-rail-collapsed"
              : ""
        }`}
      >
        <aside
          className={`tag-rail${railApplies ? "" : " tag-rail-empty"}${
            railCollapsed ? " tag-rail-collapsed" : ""
          }`}
          id="workspace-sidebar"
        >
          {/* Collapsed, the sidebar keeps its destinations as a glyph rail
              instead of leaving the screen: the counts and the tag list are
              what there is no room for, not the navigation. CSS shows exactly
              one of these two, so only one reaches the accessibility tree. */}
          <nav aria-label="Workspace navigation" className="rail-glyphs">
            <button
              aria-current={
                view === "library" && filter === "active" && !favoriteOnly ? "page" : undefined
              }
              onClick={() => {
                navigateToView("library");
                setFilter("active");
                setFavoriteOnly(false);
                setSelectedTags([]);
              }}
              title="All saves"
              type="button"
            >
              <span aria-hidden="true">▣</span>
              <span className="sr-only">All saves</span>
            </button>
            <button
              aria-current={view === "library" && favoriteOnly ? "page" : undefined}
              onClick={() => {
                navigateToView("library");
                setFilter("active");
                setFavoriteOnly(true);
                setSelectedTags([]);
              }}
              title="Favorites"
              type="button"
            >
              <span aria-hidden="true">★</span>
              <span className="sr-only">Favorites</span>
            </button>
            <button
              aria-current={view === "library" && filter === "deleted" ? "page" : undefined}
              onClick={() => {
                navigateToView("library");
                setFilter("deleted");
                setFavoriteOnly(false);
                setSelectedTags([]);
              }}
              title="Archive"
              type="button"
            >
              <span aria-hidden="true">▤</span>
              <span className="sr-only">Archive</span>
            </button>

            <span aria-hidden="true" className="rail-glyph-divider" />

            <button
              aria-current={view === "zen" ? "page" : undefined}
              onClick={() => {
                setOpenZen(null);
                navigateToView("zen");
              }}
              title="Zen documents"
              type="button"
            >
              <span aria-hidden="true">¶</span>
              <span className="sr-only">Zen documents</span>
            </button>
            {/* Tags are a list, and a list has nowhere to go in a 52px rail, so
                the tag glyph is the way back to the full sidebar. */}
            <button
              aria-controls="workspace-sidebar"
              aria-expanded={false}
              onClick={() => setRailCollapsed(false)}
              title="Tags"
              type="button"
            >
              <span aria-hidden="true">#</span>
              <span className="sr-only">Tags — show sidebar</span>
            </button>

            <span aria-hidden="true" className="rail-glyph-spacer" />

            <button
              aria-current={view === "sync" ? "page" : undefined}
              className="rail-glyph-quiet"
              onClick={() => navigateToView("sync")}
              title="Sync"
              type="button"
            >
              <span aria-hidden="true">↻</span>
              <span className="sr-only">Sync</span>
            </button>
            <button
              aria-current={view === "settings" ? "page" : undefined}
              className="rail-glyph-quiet"
              onClick={() => navigateToView("settings")}
              title="Settings"
              type="button"
            >
              <span aria-hidden="true">⚙</span>
              <span className="sr-only">Settings</span>
            </button>
          </nav>

          <div className="rail-full">
            <nav aria-label="Library views" className="rail-nav">
              <button
                aria-current={view === "library" && filter === "active" && !favoriteOnly ? "page" : undefined}
                onClick={() => {
                  navigateToView("library");
                  setFilter("active");
                  setFavoriteOnly(false);
                  setSelectedTags([]);
                }}
                type="button"
              >
                <span>All saves</span><small>{activeCount}</small>
              </button>
              <button
                aria-current={view === "library" && favoriteOnly ? "page" : undefined}
                onClick={() => {
                  navigateToView("library");
                  setFilter("active");
                  setFavoriteOnly(true);
                  setSelectedTags([]);
                }}
                type="button"
              >
                <span>★ Favorites</span>
                <small>{items.filter((item) => !item.deleted && item.favorite).length}</small>
              </button>
              <button
                aria-current={view === "library" && filter === "deleted" ? "page" : undefined}
                onClick={() => {
                  navigateToView("library");
                  setFilter("deleted");
                  setFavoriteOnly(false);
                  setSelectedTags([]);
                }}
                type="button"
              >
                <span>Archive</span><small>{deletedCount}</small>
              </button>
              {/* The strip is the phone's whole navigation, and the heading row
                  that carries the desktop's List/Graph control is not on it. */}
              <button
                aria-current={
                  view === "library" && libraryMode === "graph" ? "page" : undefined
                }
                className="mobile-graph-toggle"
                onClick={() => {
                  navigateToView("library");
                  changeLibraryMode(libraryMode === "graph" ? "list" : "graph");
                }}
                type="button"
              >
                <span>Graph</span>
              </button>
            </nav>

            <div className="rail-tags">
              <div className="rail-heading">
                <p>Zen</p>
              </div>
              <nav aria-label="Zen" className="rail-nav">
                <button
                  aria-current={view === "zen" ? "page" : undefined}
                  onClick={() => {
                    // Closes the open document too, or this reads as a dead
                    // control for anyone already inside one.
                    setOpenZen(null);
                    navigateToView("zen");
                  }}
                  type="button"
                >
                  {/* The rail heading says ZEN above it; the chip strip drops
                      headings, so the chip has to name itself. */}
                  <span className="rail-label-wide">Documents</span>
                  <span className="rail-label-narrow">Zen</span>
                  <small>{zenDocuments.length}</small>
                </button>
              </nav>
            </div>

            {/* Tags belong to the library, but they stay reachable from Zen: on a
                phone this strip is the whole navigation, and a filter that
                disappears when a document opens is a filter nobody trusts. */}
            <div className="rail-tags">
              <div className="rail-heading">
                <p>Tags</p>
                {/* With the omnibar carrying search and saving, this is the way
                    into the filters the strip has no room for. */}
                <button
                  aria-controls="library-filters"
                  aria-expanded={filtersOpen}
                  onClick={() => {
                    const toggleInPlace = view === "library" && readerItem === null;
                    navigateToView("library");
                    setFiltersOpen(toggleInPlace ? !filtersOpen : true);
                  }}
                  type="button"
                >
                  {appliedFilterCount > 0
                    ? `filters · ${appliedFilterCount}`
                    : hiddenTagCount > 0
                      ? `+${hiddenTagCount} more`
                      : "manage"}
                </button>
              </div>
              <div className="rail-tag-list">
                {railTags.map(({ count, tag }) => (
                  <button
                    aria-current={selectedTags.includes(tag) ? "page" : undefined}
                    key={tag}
                    onClick={() => toggleTagFilter(tag)}
                    type="button"
                  >
                    <span>#{tag}</span><small>{count}</small>
                  </button>
                ))}
              </div>
            </div>

            <nav aria-label="Workspace utilities" className="rail-utilities">
              <button
                aria-current={view === "sync" ? "page" : undefined}
                onClick={() => navigateToView("sync")}
                type="button"
              >
                <span>Sync</span><small>{libraryState.pendingCount} pending</small>
              </button>
              <button
                aria-current={view === "settings" ? "page" : undefined}
                onClick={() => navigateToView("settings")}
                type="button"
              >
                <span>Settings</span>
              </button>
            </nav>
          </div>
        </aside>

        <main id="workspace" tabIndex={-1}>
          <h1 className="sr-only">ResearchPocket owner library</h1>

        {readerItem ? (
          <ReaderView
            busy={busyAction !== null}
            item={items.find((item) => item.id === readerItem.id) ?? readerItem}
            onBack={closeReader}
            onDelete={deleteItem}
            onEdit={openEditor}
            onFavorite={toggleFavorite}
          />
        ) : null}

        <SyncPanel
          activeItemIds={new Set(items.filter((item) => !item.deleted).map((item) => item.id))}
          busy={busyAction !== null}
          hidden={view !== "sync" || readerItem !== null}
          onDeletePendingItem={(itemId) => {
            const item = items.find((candidate) => candidate.id === itemId);
            if (item && !item.deleted) void deleteItem(item);
          }}
          pendingChanges={libraryState.pendingChanges}
          state={syncState}
        />

        <SettingsPanel
          activeProfileId={activeProfileId}
          density={density}
          hidden={view !== "settings" || readerItem !== null}
          imageBackground={imageBackground}
          onDensityChange={setDensity}
          onImageBackgroundChange={setImageBackground}
          onProfilesChanged={refreshProfiles}
          onReaderMeasureChange={setReaderMeasure}
          onSwitchLibrary={switchLibrary}
          onThemeChange={setTheme}
          profiles={profiles}
          readerMeasure={readerMeasure}
          theme={theme}
        />

        <ZenWorkspace
          busy={busyAction !== null}
          documents={zenDocuments}
          hidden={view !== "zen" || readerItem !== null}
          onClose={closeZenDocument}
          onCreate={(title) => void createZenDocument(title)}
          onDelete={(documentId) => void removeZenDocument(documentId)}
          onLeave={() => navigateToView("library")}
          onOpen={(documentId) => void openZenDocument(documentId)}
          onSaveBody={(documentId, body) => void saveZenBody(documentId, body)}
          onSaveTitle={(documentId, title) => void saveZenTitle(documentId, title)}
          open={openZen}
          resolveMention={resolveZenMention}
        />

          <section
          aria-busy={searchPending}
          aria-labelledby="library-heading"
          className="library-section"
          hidden={view !== "library" || readerItem !== null}
          id="library"
        >
          <div className="library-heading-row">
            <div className="library-title">
              <h2 id="library-heading">
                {favoriteOnly ? "Favorites" : filter === "deleted" ? "Archive" : selectedTags.length === 1 ? `#${selectedTags[0]}` : "All saves"}
              </h2>
              <p className="library-count">{pluralize(visibleItems.length, "item")}</p>
            </div>
            <div className="library-view-controls">
              {/* Order is a list idea, but its reserved space keeps the view
                  switch anchored when the graph is selected. */}
              <label
                aria-hidden={libraryMode !== "list"}
                className={`sort-control${libraryMode === "list" ? "" : " sort-control-hidden"}`}
              >
                <span className="sr-only">Sort order</span>
                <select
                  disabled={libraryMode !== "list"}
                  onChange={(event) => setSortMode(event.target.value as SortMode)}
                  value={sortMode}
                >
                  <option value="recent">newest ↓</option>
                  <option value="oldest">oldest ↑</option>
                  <option value="title">title A–Z</option>
                </select>
              </label>
              <div aria-label="Library projection" className="library-mode" role="group">
                <button
                  aria-pressed={libraryMode === "list"}
                  onClick={() => changeLibraryMode("list")}
                  type="button"
                >
                  List
                </button>
                <button
                  aria-pressed={libraryMode === "graph"}
                  onClick={() => changeLibraryMode("graph")}
                  type="button"
                >
                  Graph
                </button>
              </div>
            </div>
          </div>

          <div className="library-search-row">
            <Omnibar
              busy={busyAction !== null}
              onAdd={addItem}
              onQueryChange={setQuery}
              query={query}
            />
            <button
              aria-controls="library-filters"
              aria-expanded={filtersOpen}
              className="filter-trigger"
              onClick={() => setFiltersOpen((open) => !open)}
              type="button"
            >
              <svg aria-hidden="true" viewBox="0 0 20 20">
                <path d="M3 5h14M6 10h8M8.5 15h3" />
              </svg>
              <span>Filters</span>
              {appliedFilterCount > 0 ? (
                <span className="filter-trigger-count">{appliedFilterCount}</span>
              ) : null}
            </button>
          </div>

          <div
            aria-label="Library filters"
            className="library-filters"
            hidden={!filtersOpen}
            id="library-filters"
            role="region"
          >
            <div className="filter-controls">
              <label>
                <select
                  aria-label="Search fields"
                  id="search-scope"
                  name="search-scope"
                  onChange={(event) => setSearchScope(event.target.value as SearchScope)}
                  value={searchScope}
                >
                  <option value="all">All fields</option>
                  <option value="title">Title</option>
                  <option value="url">URL</option>
                  <option value="context">Context</option>
                  <option value="tags">Tags</option>
                </select>
              </label>
              <label>
                <select
                  aria-label="Item state"
                  id="lifecycle-filter"
                  name="lifecycle-filter"
                  onChange={(event) => setFilter(event.target.value as LifecycleFilter)}
                  value={filter}
                >
                  <option value="active">Active {activeCount}</option>
                  <option value="deleted">Deleted {deletedCount}</option>
                  <option value="all">All {items.length}</option>
                </select>
              </label>
              <label>
                <select
                  aria-label="Sort order"
                  id="sort-mode"
                  name="sort-mode"
                  onChange={(event) => setSortMode(event.target.value as SortMode)}
                  value={sortMode}
                >
                  <option value="recent">Newest</option>
                  <option value="oldest">Oldest</option>
                  <option value="title">Title</option>
                </select>
              </label>
              <label className="filter-favorite">
                <input
                  checked={favoriteOnly}
                  id="favorite-filter"
                  name="favorite-filter"
                  onChange={(event) => setFavoriteOnly(event.target.checked)}
                  type="checkbox"
                />
                <span>Favorites</span>
              </label>
              <div className="filter-actions">
                {appliedFilterCount > 0 ? (
                  <button
                    className="filter-reset"
                    onClick={() => {
                      setFilter("active");
                      setSearchScope("all");
                      setFavoriteOnly(false);
                      setSelectedTags([]);
                      setTagSearch("");
                      setTagMatchMode("all");
                      setSortMode("recent");
                    }}
                    type="button"
                  >
                    Clear
                  </button>
                ) : null}
                <button
                  aria-label="Close filters"
                  className="filter-close"
                  onClick={() => setFiltersOpen(false)}
                  title="Close filters"
                  type="button"
                >
                  <span aria-hidden="true">×</span>
                </button>
              </div>
            </div>
            {knownTags.length > 0 ? (
              <fieldset aria-label="Tag filters" className="tag-filter-group">
                <legend className="sr-only">Tags</legend>
                <label className="tag-filter-search">
                  <input
                    aria-label="Find tags"
                    autoComplete="off"
                    id="tag-filter-search"
                    name="tag-filter-search"
                    onChange={(event) => setTagSearch(event.target.value)}
                    placeholder="Find a tag…"
                    type="search"
                    value={tagSearch}
                  />
                </label>
                <div aria-label="Tag filter selection" className="tag-filter-options">
                  {selectedTags.map((tag) => (
                    <button
                      aria-label={`${tag} ×, remove tag filter`}
                      aria-pressed="true"
                      key={tag}
                      onClick={() =>
                        setSelectedTags((current) =>
                          current.filter((value) => value !== tag),
                        )
                      }
                      type="button"
                    >
                      {tag} ×
                    </button>
                  ))}
                  {tagFilterMatches.map((tag) => (
                    <button
                      aria-label={`+ ${tag}, add tag filter`}
                      aria-pressed="false"
                      key={tag}
                      onClick={() => {
                        setSelectedTags((current) => [...current, tag]);
                        setTagSearch("");
                      }}
                      type="button"
                    >
                      + {tag}
                    </button>
                  ))}
                  {tagSearch.trim() && tagFilterMatches.length === 0 ? (
                    <span className="tag-filter-empty">No matching tags</span>
                  ) : null}
                </div>
                {selectedTags.length > 1 ? (
                  <label>
                    <select
                      aria-label="Tag matching"
                      id="tag-match-mode"
                      name="tag-match-mode"
                      onChange={(event) =>
                        setTagMatchMode(event.target.value as TagMatchMode)
                      }
                      value={tagMatchMode}
                    >
                      <option value="all">All selected tags</option>
                      <option value="any">Any selected tag</option>
                    </select>
                  </label>
                ) : null}
              </fieldset>
            ) : null}
          </div>

          <p className="result-count">
            {searchPending ? "Updating…" : pluralize(visibleItems.length, "result")}
          </p>

          {visibleItems.length === 0 ? (
            <EmptyLibrary filter={filter} hasQuery={query.trim().length > 0} />
          ) : libraryMode === "graph" ? (
            <LibraryGraph
              busy={busyAction !== null}
              hidden={view !== "library" || readerItem !== null}
              items={visibleItems}
              mentions={zenMentions}
              onDeleteItem={(itemId) => {
                const item = items.find((candidate) => candidate.id === itemId);
                if (item) void deleteItem(item);
              }}
              onEditItem={(itemId, opener) => {
                const item = items.find((candidate) => candidate.id === itemId);
                if (item) openEditor(item, opener);
              }}
              onFilterTag={toggleTagFilter}
              onFavoriteItem={(itemId) => {
                const item = items.find((candidate) => candidate.id === itemId);
                if (item) void toggleFavorite(item);
              }}
              onOpenItem={(itemId) => {
                const item = items.find((candidate) => candidate.id === itemId);
                if (item) openReader(item);
              }}
              onRestoreItem={(itemId) => {
                const item = items.find((candidate) => candidate.id === itemId);
                if (item) void restoreItem(item);
              }}
              selectedTags={selectedTags}
            />
          ) : (
            <ol className={`item-list item-list-${density}`}>
              {renderedItems.map((item) => (
                <LibraryItem
                  busy={busyAction !== null}
                  item={item}
                  key={item.id}
                  onDelete={deleteItem}
                  onEdit={openEditor}
                  onFavorite={toggleFavorite}
                  onRead={openReader}
                  onRestore={restoreItem}
                  onToggleTagFilter={toggleTagFilter}
                  selectedTags={selectedTags}
                />
              ))}
            </ol>
          )}
          {libraryMode === "list" && renderedItems.length < visibleItems.length ? (
            <button
              className="list-more"
              onClick={() => setVisibleLimit((current) => current + LIST_BATCH_SIZE)}
              type="button"
            >
              Show {Math.min(LIST_BATCH_SIZE, visibleItems.length - renderedItems.length)} more
            </button>
          ) : null}
          <div aria-atomic="true" aria-live="polite" className="sr-only">
            {searchPending
              ? "Updating library results."
              : libraryMode === "graph"
                ? `Graphing ${pluralize(visibleItems.length, "result")}`
                : `Showing ${renderedItems.length.toLocaleString()} of ${pluralize(visibleItems.length, "result")}`}
          </div>
          <footer className="keyboard-footer">
            <span><b>/</b> search</span>
            <span><b>⌘V</b> save a URL</span>
            <span><b>⏎</b> reader</span>
            {libraryMode === "graph" ? <span><b>0</b> fit all</span> : null}
            {libraryMode === "graph" ? <span><b>+/-</b> zoom</span> : null}
            {libraryMode === "graph" ? <span><b>C</b> focus</span> : null}
          </footer>
          </section>
        </main>
        {/* Zen moved into the chip strip, so this keeps only what the strip and
            the omnibar cannot carry: an authored capture, and Settings. It is
            gone entirely under a pushed screen, which has its own footer. */}
        {pushedScreen || readerItem ? null : (
          <nav aria-label="Mobile actions" className="mobile-actions">
            <button className="primary-button" onClick={(event) => openCapture(event.currentTarget)} type="button">＋ Save a link</button>
            <button className="secondary-button" onClick={() => navigateToView("settings")} type="button">Settings</button>
          </nav>
        )}
      </div>

      <div aria-atomic="true" aria-live="polite" className="sr-only">
        {announcement}
      </div>

      {undoNotice ? (
        <div className="undo-notice" role="status">
          <span>{undoNotice.message}</span>
          <button
            disabled={busyAction !== null}
            onClick={() => void undoLastChange()}
            type="button"
          >
            Undo
          </button>
          <button
            aria-label="Dismiss undo"
            className="undo-dismiss"
            disabled={busyAction !== null}
            onClick={() => setUndoNotice(null)}
            type="button"
          >
            ×
          </button>
        </div>
      ) : null}

      {displayedError && !editingItem && !capturing ? (
        <div className="error-banner" role="alert">
          <strong>Something needs your attention.</strong>
          <span>{displayedError}</span>
          {localError ? (
            <button onClick={() => setLocalError(null)} type="button">
              Dismiss
            </button>
          ) : null}
        </div>
      ) : null}

      {editingItem ? (
          <EditDialog
            busy={busyAction !== null}
            error={localError}
            item={editingItem}
            knownTags={knownTags}
          onClose={closeEditor}
          onSave={saveEdit}
        />
      ) : null}

      {capturing ? (
        <CaptureDialog
          busy={busyAction !== null}
          error={localError}
          knownTags={knownTags}
          onAdd={addItem}
          onClose={closeCapture}
        />
      ) : null}

      {commandOpen ? (
        <CommandPalette
          items={items.filter((item) => !item.deleted)}
          libraries={profiles.filter((profile) => profile.id !== activeProfileId)}
          onClose={() => setCommandOpen(false)}
          onFilterTag={(tag) => {
            navigateToView("library");
            setSelectedTags([tag]);
            setCommandOpen(false);
          }}
          onNewLibrary={() => {
            setCommandOpen(false);
            void createLibrary();
          }}
          onNewSave={() => {
            setCommandOpen(false);
            setCapturing(true);
          }}
          onNewZenDocument={() => {
            setCommandOpen(false);
            navigateToView("zen");
            void createZenDocument("");
          }}
          onOpenItem={(item) => {
            openReader(item);
            setCommandOpen(false);
          }}
          onOpenZenDocument={(documentId) => {
            setCommandOpen(false);
            navigateToView("zen");
            void openZenDocument(documentId);
          }}
          onOpenZenWorkspace={() => {
            setCommandOpen(false);
            setOpenZen(null);
            navigateToView("zen");
          }}
          onSwitchLibrary={(profileId) => {
            setCommandOpen(false);
            void switchLibrary(profileId);
          }}
          zenDocuments={zenDocuments}
          onSync={() => {
            navigateToView("sync");
            setCommandOpen(false);
          }}
          tags={tagCounts}
        />
      ) : null}

    </div>
  );
}

function BootScreen() {
  return (
    <main aria-busy="true" aria-live="polite" className="boot-shell">
      <span aria-hidden="true" className="welcome-monogram">rp</span>
      <p className="eyebrow">ResearchPocket / owner app</p>
      <h1>Opening this browser's library…</h1>
    </main>
  );
}

function Welcome({
  busy,
  error,
  onInitialize,
  onRestore,
  restoreFirst,
}: {
  busy: boolean;
  error: string | null;
  onInitialize: () => Promise<void>;
  onRestore: () => Promise<void>;
  restoreFirst: boolean;
}) {
  useEffect(() => {
    if (busy) return;

    function handleShortcut(event: KeyboardEvent) {
      const target = event.target as HTMLElement | null;
      const isInteractive =
        target?.matches("button, a, input, textarea, select") ||
        target?.isContentEditable;
      if (
        isInteractive ||
        event.altKey ||
        event.ctrlKey ||
        event.metaKey ||
        event.shiftKey
      ) {
        return;
      }

      if (event.key === "Enter") {
        event.preventDefault();
        void onInitialize();
      } else if (event.key.toLowerCase() === "r") {
        event.preventDefault();
        void onRestore();
      }
    }

    window.addEventListener("keydown", handleShortcut);
    return () => window.removeEventListener("keydown", handleShortcut);
  }, [busy, onInitialize, onRestore]);

  return (
    <main className="welcome-shell">
      <section aria-labelledby="welcome-heading" className="welcome-card">
        <div aria-hidden="true" className="welcome-monogram">
          rp
        </div>
        <h1 id="welcome-heading">
          {restoreFirst
            ? "Bring your library into this browser."
            : "Set up this browser."}
        </h1>
        <p className="welcome-copy">
          No library is stored in this browser yet. Create an empty local library
          or restore one you already use.
        </p>

        {error ? <p role="alert">{error}</p> : null}

        <div className="onboarding-options">
          <button className={!restoreFirst ? "onboarding-choice onboarding-choice-priority" : "onboarding-choice"} disabled={busy} onClick={() => void onInitialize()} type="button">
            <span aria-hidden="true">&gt;</span>
            <span><strong>Create a local library</strong><small>Start empty in this browser. Connect private sync whenever.</small></span>
            <kbd>↵</kbd>
          </button>
          <button className={restoreFirst ? "onboarding-choice onboarding-choice-priority" : "onboarding-choice"} disabled={busy} onClick={() => void onRestore()} type="button">
            <span aria-hidden="true">&gt;</span>
            <span><strong>Restore an existing library</strong><small>Pull the library your CLI or another browser already syncs.</small></span>
            <kbd>R</kbd>
          </button>
        </div>

        <p className="welcome-footnote">
          Restoring first avoids merges. Keep browser storage enabled. <a href="../">Product overview</a>
        </p>
      </section>
    </main>
  );
}

function LibrarySettings({
  activeProfileId,
  onProfilesChanged,
  onSwitchLibrary,
  profiles,
}: {
  activeProfileId: string | null;
  onProfilesChanged: () => Promise<void>;
  onSwitchLibrary: (profileId: string) => Promise<void>;
  profiles: LibraryProfile[];
}) {
  const [newName, setNewName] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  async function run(work: () => Promise<unknown>) {
    setBusy(true);
    setError(null);
    try {
      await work();
      await onProfilesChanged();
    } catch (caught) {
      setError(readError(caught));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="settings-group">
      <div>
        <h3>Libraries</h3>
      </div>

      <div className="library-settings">
        <ul className="library-list">
          {profiles.map((profile) => {
            const current = profile.id === activeProfileId;
            return (
              <li className="library-row" key={profile.id}>
                <div className="library-row-copy">
                  <p>
                    {profile.name}
                    {current ? <span className="status-label"> open</span> : null}
                  </p>
                  <p>
                    {profile.lastOpenedAt
                      ? `Last opened ${formatDateTime(profile.lastOpenedAt)}`
                      : "Not opened yet"}
                  </p>
                </div>
                <div className="library-row-actions">
                  {current ? null : (
                    <button
                      disabled={busy}
                      onClick={() => void onSwitchLibrary(profile.id)}
                      type="button"
                    >
                      Open
                    </button>
                  )}
                  <button
                    disabled={busy}
                    onClick={() =>
                      void run(async () => {
                        const name = window.prompt("Rename library", profile.name);
                        if (name === null) return;
                        await renameProfile(profile.id, name);
                      })
                    }
                    type="button"
                  >
                    Rename
                  </button>
                  <button
                    disabled={busy || profiles.length === 1}
                    onClick={() =>
                      void run(async () => {
                        const confirmed = window.confirm(
                          `Delete the local copy of "${profile.name}"? Its GitHub repository is not touched, so you can reconnect to restore it.`,
                        );
                        if (!confirmed) return;
                        const next = await deleteProfile(profile.id);
                        if (profile.id === activeProfileId) await onSwitchLibrary(next.id);
                      })
                    }
                    type="button"
                  >
                    Delete local copy
                  </button>
                </div>
              </li>
            );
          })}
        </ul>

        <form
          className="library-create"
          onSubmit={(event) => {
            event.preventDefault();
            void run(async () => {
              const created = await createProfile(newName);
              setNewName("");
              await onSwitchLibrary(created.id);
            });
          }}
        >
          <label className="field">
            <span>New library</span>
            <input
              autoCapitalize="none"
              autoComplete="off"
              name="library-name"
              onChange={(event) => setNewName(event.target.value)}
              placeholder="Team research"
              required
              value={newName}
            />
          </label>
          <button className="secondary-button" disabled={busy} type="submit">
            Create and open
          </button>
        </form>

        {error ? <p className="sync-error" role="alert">{error}</p> : null}
      </div>
    </div>
  );
}

function SettingsPanel({
  activeProfileId,
  density,
  hidden,
  imageBackground,
  onDensityChange,
  onImageBackgroundChange,
  onProfilesChanged,
  onReaderMeasureChange,
  onSwitchLibrary,
  onThemeChange,
  profiles,
  readerMeasure,
  theme,
}: {
  activeProfileId: string | null;
  density: Density;
  hidden: boolean;
  imageBackground: string;
  onDensityChange: (density: Density) => void;
  onImageBackgroundChange: (color: string) => void;
  onProfilesChanged: () => Promise<void>;
  onReaderMeasureChange: (measure: ReaderMeasure) => void;
  onSwitchLibrary: (profileId: string) => Promise<void>;
  onThemeChange: (theme: ThemeColors) => void;
  profiles: LibraryProfile[];
  readerMeasure: ReaderMeasure;
  theme: ThemeColors;
}) {
  const selectedPreset =
    THEME_PRESETS.find(({ colors }) => themesEqual(theme, colors))?.id ?? "custom";

  return (
    <section
      aria-labelledby="settings-heading"
      className="settings-section"
      hidden={hidden}
      id="settings"
    >
      <header className="settings-heading">
        <p className="eyebrow">Workspace</p>
        <h2 id="settings-heading">Settings</h2>
      </header>

      <LibrarySettings
        activeProfileId={activeProfileId}
        onProfilesChanged={onProfilesChanged}
        onSwitchLibrary={onSwitchLibrary}
        profiles={profiles}
      />

      <div className="settings-group">
        <div>
          <h3>Appearance</h3>
        </div>
        <div className="appearance-settings">
          <label className="setting-row" htmlFor="compact-mode">
            <span>
              <strong>Compact mode</strong>
            </span>
            <input
              checked={density === "compact"}
              id="compact-mode"
              onChange={(event) =>
                onDensityChange(event.target.checked ? "compact" : "comfortable")
              }
              type="checkbox"
            />
          </label>
          <label className="setting-row" htmlFor="reader-measure">
            <span>
              <strong>Reading width</strong>
              <small>
                How far a line of text runs in an opened save. Anything short of
                full width is centered in the column.
              </small>
            </span>
            <select
              id="reader-measure"
              onChange={(event) =>
                onReaderMeasureChange(event.target.value as ReaderMeasure)
              }
              value={readerMeasure}
            >
              {READER_MEASURES.map(({ id, label }) => (
                <option key={id} value={id}>{label}</option>
              ))}
            </select>
          </label>
          <fieldset className="theme-editor">
            <legend>Color theme</legend>
            <label className="theme-preset" htmlFor="theme-preset">
              <span>
                <strong>Theme preset</strong>
              </span>
              <select
                id="theme-preset"
                onChange={(event) => {
                  const preset = THEME_PRESETS.find(
                    ({ id }) => id === event.target.value,
                  );
                  if (preset) onThemeChange({ ...preset.colors });
                }}
                value={selectedPreset}
              >
                {selectedPreset === "custom" ? (
                  <option value="custom">Custom</option>
                ) : null}
                {THEME_PRESETS.map(({ id, label }) => (
                  <option key={id} value={id}>{label}</option>
                ))}
              </select>
            </label>
            <div className="theme-color-grid">
              {THEME_FIELDS.map(({ key, label }) => (
                <label htmlFor={`theme-${key}`} key={key}>
                  <span>{label}</span>
                  <input
                    id={`theme-${key}`}
                    onChange={(event) =>
                      onThemeChange({ ...theme, [key]: event.target.value })
                    }
                    type="color"
                    value={theme[key]}
                  />
                  <code>{theme[key]}</code>
                </label>
              ))}
            </div>
            <button
              className="secondary-button theme-reset"
              disabled={themesEqual(theme, DEFAULT_THEME)}
              onClick={() => onThemeChange({ ...DEFAULT_THEME })}
              type="button"
            >
              Reset to default theme
            </button>
          </fieldset>
          <fieldset className="theme-editor image-background-editor">
            <legend>Transparent image background</legend>
            <label htmlFor="image-background">
              <span>Color</span>
              <input
                id="image-background"
                onChange={(event) =>
                  onImageBackgroundChange(event.target.value)
                }
                type="color"
                value={imageBackground}
              />
              <code>{imageBackground}</code>
            </label>
            <button
              className="secondary-button theme-reset"
              disabled={imageBackground === DEFAULT_IMAGE_BACKGROUND}
              onClick={() =>
                onImageBackgroundChange(DEFAULT_IMAGE_BACKGROUND)
              }
              type="button"
            >
              Reset image background
            </button>
          </fieldset>
        </div>
      </div>

      <div className="settings-group">
        <div>
          <h3>Keyboard</h3>
        </div>
        <dl className="shortcut-list">
          <div><dt>Command palette</dt><dd><kbd>Ctrl Shift P</kbd></dd></div>
          <div><dt>Search library</dt><dd><kbd>/</kbd></dd></div>
          <div><dt>Close a focused view</dt><dd><kbd>Esc</kbd></dd></div>
        </dl>
      </div>

      <div className="settings-group">
        <div>
          <h3>Local data</h3>
        </div>
        <p className="settings-status"><span aria-hidden="true" className="status-dot" /> Browser storage enabled</p>
      </div>
    </section>
  );
}

function SyncPanel({
  activeItemIds,
  busy,
  hidden,
  onDeletePendingItem,
  pendingChanges,
  state,
}: {
  activeItemIds: ReadonlySet<string>;
  busy: boolean;
  hidden: boolean;
  onDeletePendingItem: (itemId: string) => void;
  pendingChanges: PendingSyncChange[];
  state: BrowserSyncState;
}) {
  const [repository, setRepository] = useState("");
  const [branch, setBranch] = useState("");
  const [formError, setFormError] = useState<string | null>(null);

  async function connect(event: SubmitEvent<HTMLFormElement>) {
    event.preventDefault();
    const form = event.currentTarget;
    const credential = readSyncCredential(form);
    setFormError(null);
    try {
      await browserSync.connect({ repository, branch, ...credential });
      form.reset();
    } catch (error) {
      setFormError(readError(error));
    } finally {
      clearCredentialInput(form);
    }
  }

  async function unlock(event: SubmitEvent<HTMLFormElement>) {
    event.preventDefault();
    const form = event.currentTarget;
    const credential = readSyncCredential(form);
    setFormError(null);
    try {
      await browserSync.unlock(credential);
      form.reset();
    } catch (error) {
      setFormError(readError(error));
    } finally {
      clearCredentialInput(form);
    }
  }

  async function syncNow() {
    setFormError(null);
    try {
      await browserSync.syncNow();
    } catch (error) {
      setFormError(readError(error));
    }
  }

  const error = formError ?? state.error;
  const remote = state.configuration;

  return (
    <section
      aria-labelledby="sync-heading"
      className="sync-section"
      hidden={hidden}
    >
      <header className="sync-header">
        <div>
          <h2 id="sync-heading">Sync</h2>
          <p>Private GitHub repository</p>
        </div>
        <div className="sync-state" role="status">
          <span aria-hidden="true" className="status-dot" />
          <span>{state.status}</span>
        </div>
      </header>

      <div className="sync-layout">
        <section aria-labelledby="sync-connection-heading" className="sync-connection">
          <div className="sync-panel-heading">
            <h3 id="sync-connection-heading">
              {remote ? "Connection" : "Connect GitHub"}
            </h3>
            {remote ? <span>{remote.owner}/{remote.repository}</span> : null}
          </div>

          {remote ? (
            <dl className="sync-facts">
              <div>
                <dt>Repository</dt>
                <dd>{remote.owner}/{remote.repository}</dd>
              </div>
              <div>
                <dt>Branch</dt>
                <dd>{remote.branch}</dd>
              </div>
              <div>
                <dt>Last sync</dt>
                <dd>
                  {remote.lastSuccessAt ? formatDateTime(remote.lastSuccessAt) : "Never"}
                </dd>
              </div>
              {state.lastCycle ? (
                <div>
                  <dt>Last cycle</dt>
                  <dd>
                    {state.lastCycle.downloaded + state.lastCycle.aggregatesApplied} down ·{" "}
                    {state.lastCycle.uploaded + state.lastCycle.aggregatesUploaded} up
                  </dd>
                </div>
              ) : null}
            </dl>
          ) : null}

          {!remote ? (
            <form className="sync-form" onSubmit={(event) => void connect(event)}>
              <div className="sync-repository-fields">
                <label className="field">
                  <span>Private repository</span>
                  <input
                    autoCapitalize="none"
                    autoComplete="off"
                    id="sync-repository"
                    name="repository"
                    onChange={(event) => setRepository(event.target.value)}
                    placeholder="owner/private-repository"
                    required
                    spellCheck={false}
                    value={repository}
                  />
                </label>
                <label className="field">
                  <span>Branch <small>optional</small></span>
                  <input
                    autoCapitalize="none"
                    autoComplete="off"
                    id="sync-branch"
                    name="branch"
                    onChange={(event) => setBranch(event.target.value)}
                    placeholder="main"
                    spellCheck={false}
                    value={branch}
                  />
                </label>
              </div>
              <TokenFields />
              {error ? <p className="sync-error" role="alert">{error}</p> : null}
              <button className="primary-button" disabled={state.syncing} type="submit">
                {state.syncing ? "Connecting…" : "Connect"}
              </button>
            </form>
          ) : !state.credentialAvailable ? (
            <form className="sync-form" onSubmit={(event) => void unlock(event)}>
              <p className="sync-connection-note">
                Repository saved. Enter the token again to pull and push.
              </p>
              <TokenFields />
              {error ? <p className="sync-error" role="alert">{error}</p> : null}
              <button className="primary-button" disabled={state.syncing} type="submit">
                {state.syncing ? "Synchronizing…" : "Unlock and sync"}
              </button>
            </form>
          ) : (
            <div className="sync-controls">
              <p>Token available only in this browser context.</p>
              {error ? <p className="sync-error" role="alert">{error}</p> : null}
              <div className="sync-actions">
                <button
                  className="primary-button"
                  disabled={state.syncing}
                  onClick={() => void syncNow()}
                  type="button"
                >
                  {state.syncing ? "Synchronizing…" : "Sync now"}
                </button>
                <button
                  className="secondary-button"
                  disabled={state.syncing}
                  onClick={() => browserSync.forgetCredential()}
                  type="button"
                >
                  Forget token
                </button>
              </div>
            </div>
          )}
        </section>

        <PendingSyncChanges
          activeItemIds={activeItemIds}
          changes={pendingChanges}
          hasSynced={Boolean(remote?.lastSuccessAt)}
          mutating={busy}
          onDeleteItem={onDeletePendingItem}
          syncing={state.syncing}
        />
      </div>
    </section>
  );
}

function PendingSyncChanges({
  activeItemIds,
  changes,
  hasSynced,
  mutating,
  onDeleteItem,
  syncing,
}: {
  activeItemIds: ReadonlySet<string>;
  changes: PendingSyncChange[];
  hasSynced: boolean;
  mutating: boolean;
  onDeleteItem: (itemId: string) => void;
  syncing: boolean;
}) {
  return (
    <section
      aria-busy={syncing}
      aria-labelledby="sync-pending-heading"
      className="sync-pending"
    >
      <div className="sync-pending-heading">
        <h3 id="sync-pending-heading">Pending</h3>
        <span>{pluralize(changes.length, "change")}</span>
      </div>
      <p className="sync-pending-help">
        Kept locally until GitHub confirms them.
      </p>

      {changes.length > 0 ? (
        <ol className="sync-change-list">
          {changes.map((change) => (
            <li className="sync-change" key={change.path}>
              <span className="sync-change-kind">{pendingChangeAction(change.kind)}</span>
              <div className="sync-change-copy">
                <p>{change.label}</p>
                <p>{pendingChangeDetails(change)}</p>
              </div>
              <time dateTime={change.enqueuedAt}>{formatDateTime(change.enqueuedAt)}</time>
              {change.kind === "create" && change.itemId && activeItemIds.has(change.itemId) ? (
                <button
                  aria-label={`Delete pending item ${change.label}`}
                  className="sync-change-delete"
                  disabled={syncing || mutating}
                  onClick={() => onDeleteItem(change.itemId!)}
                  type="button"
                >
                  Delete item
                </button>
              ) : null}
            </li>
          ))}
        </ol>
      ) : (
        <p className="sync-pending-empty">
          {hasSynced
            ? "Everything is synced."
            : "No changes waiting."}
        </p>
      )}
    </section>
  );
}

function TokenFields() {
  return (
    <>
      <label className="field sync-token-field">
        <span>Fine-grained GitHub token</span>
        <input
          autoCapitalize="none"
          autoComplete="off"
          id="github-token"
          name="github-token"
          required
          spellCheck={false}
          type="password"
        />
      </label>
      <label className="check-field sync-session-choice">
        <input
          id="remember-for-tab"
          name="remember-for-tab"
          type="checkbox"
        />
        <span>Keep token for this tab</span>
      </label>
      <p className="sync-help">
        Expiring fine-grained token · this repository only · Contents read/write.
      </p>
    </>
  );
}

/**
 * One field for the two things a library is asked for: find something, or take
 * something in. Typing filters as you go; a pasted URL turns the same field
 * into a save, which is why it replaced a capture form, a search box and a
 * filter button standing in a row.
 *
 * A save needs the scheme. Without it "inkandswitch.com" is indistinguishable
 * from a search for that host, and guessing wrong writes an item.
 */
function Omnibar({
  busy,
  onAdd,
  onQueryChange,
  query,
}: {
  busy: boolean;
  onAdd: (input: AddInput) => Promise<boolean>;
  onQueryChange: (value: string) => void;
  query: string;
}) {
  const parsed = readOmnibarUrl(query);

  async function submit(event: SubmitEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!parsed) return;
    if (await onAdd({ favorite: false, tags: parsed.tags, url: parsed.url } as AddInput)) {
      onQueryChange("");
    }
  }

  return (
    <form className="omnibar" onSubmit={(event) => void submit(event)} role="search">
      <label className="sr-only" htmlFor="library-search">
        Search your library, or paste a URL to save it
      </label>
      <svg aria-hidden="true" className="search-glyph" viewBox="0 0 24 24">
        <circle cx="10.5" cy="10.5" r="6.5" />
        <path d="m15.5 15.5 4 4" />
      </svg>
      <input
        autoCapitalize="none"
        autoComplete="off"
        disabled={busy}
        id="library-search"
        name="query"
        onChange={(event) => onQueryChange(event.target.value)}
        placeholder="Search — or paste a URL to save it"
        type="text"
        value={query}
      />
      {parsed ? (
        <span className="omnibar-hint">
          <kbd>↵</kbd> save{parsed.tags.length > 0 ? ` · ${parsed.tags.length} tag${parsed.tags.length === 1 ? "" : "s"}` : ""}
        </span>
      ) : query ? (
        <button
          aria-label="Clear search"
          className="search-clear"
          onClick={() => onQueryChange("")}
          title="Clear search"
          type="button"
        >
          <span aria-hidden="true">×</span>
        </button>
      ) : (
        <kbd className="omnibar-key">/</kbd>
      )}
    </form>
  );
}

/** Reads an omnibar value as a save: one absolute URL plus any `#tags`. */
function readOmnibarUrl(value: string): { tags: string[]; url: string } | null {
  const parts = value.trim().split(/\s+/).filter(Boolean);
  const urls = parts.filter((part) => /^https?:\/\/\S+$/i.test(part));
  if (urls.length !== 1) return null;
  const tags = parts
    .filter((part) => part.startsWith("#") && part.length > 1)
    .map((part) => part.slice(1));
  // Anything that is neither the URL nor a tag means this is prose, not a save.
  if (parts.length !== urls.length + tags.length) return null;
  return { tags, url: urls[0]! };
}

interface CommandOption {
  key: string;
  group: string;
  primary: string;
  detail?: string;
  hint?: string;
  run(): void;
}

function CommandPalette({
  items,
  libraries,
  onClose,
  onFilterTag,
  onNewLibrary,
  onNewSave,
  onNewZenDocument,
  onOpenItem,
  onOpenZenDocument,
  onOpenZenWorkspace,
  onSwitchLibrary,
  onSync,
  tags,
  zenDocuments,
}: {
  items: LibraryItemView[];
  libraries: LibraryProfile[];
  onClose: () => void;
  onFilterTag: (tag: string) => void;
  onNewSave: () => void;
  onNewLibrary: () => void;
  onNewZenDocument: () => void;
  onOpenItem: (item: LibraryItemView) => void;
  onOpenZenDocument: (documentId: string) => void;
  onOpenZenWorkspace: () => void;
  onSwitchLibrary: (profileId: string) => void;
  onSync: () => void;
  tags: { count: number; tag: string }[];
  zenDocuments: PersistedZenDocument[];
}) {
  const [query, setQuery] = useState("");
  const [activeIndex, setActiveIndex] = useState(0);
  const dialogRef = useRef<HTMLDialogElement>(null);
  const normalized = query.trim().toLocaleLowerCase();

  // One flat list drives selection, keyboard movement, and rendering. Offsets
  // per section used to be recomputed in four places, so every new kind of
  // result was a chance to mis-index one of them.
  const options: CommandOption[] = [
    ...tags
      .filter(({ tag }) => !normalized || tag.toLocaleLowerCase().includes(normalized))
      .slice(0, 5)
      .map(({ count, tag }) => ({
        key: `tag-${tag}`,
        group: "Tags",
        primary: `#${tag}`,
        detail: pluralize(count, "save"),
        hint: "↵ filter library",
        run: () => onFilterTag(tag),
      })),
    ...items
      .filter((item) =>
        !normalized || [item.title, item.url, ...item.tags]
          .filter(Boolean)
          .some((value) => value!.toLocaleLowerCase().includes(normalized)),
      )
      .slice(0, 6)
      .map((item) => ({
        key: `item-${item.id}`,
        group: "Saves",
        primary: item.title?.trim() || item.url,
        detail: readHostname(item.url),
        hint: item.tags.map((tag) => `#${tag}`).join(" "),
        run: () => onOpenItem(item),
      })),
    ...zenDocuments
      .filter(
        (document) =>
          !normalized ||
          (document.title ?? "").toLocaleLowerCase().includes(normalized) ||
          document.tags.some((tag) => tag.toLocaleLowerCase().includes(normalized)),
      )
      .slice(0, 5)
      .map((document) => ({
        key: `zen-${document.documentId}`,
        group: "Documents",
        primary: document.title?.trim() || "Untitled document",
        detail:
          document.todoTotal > 0
            ? `${document.todoDone}/${document.todoTotal} todos`
            : "document",
        hint: "↵ open document",
        run: () => onOpenZenDocument(document.documentId),
      })),
    ...libraries
      .filter((profile) => !normalized || profile.name.toLocaleLowerCase().includes(normalized))
      .slice(0, 4)
      .map((profile) => ({
        key: `library-${profile.id}`,
        group: "Libraries",
        primary: profile.name,
        hint: "↵ open library",
        run: () => onSwitchLibrary(profile.id),
      })),
    {
      key: "action-sync",
      group: "Actions",
      primary: "Sync",
      hint: "open status",
      run: onSync,
    },
    {
      key: "action-save",
      group: "Actions",
      primary: "Save a URL",
      hint: "new save",
      run: onNewSave,
    },
    {
      key: "action-zen",
      group: "Actions",
      primary: "Zen",
      hint: "open documents",
      run: onOpenZenWorkspace,
    },
    {
      key: "action-zen-new",
      group: "Actions",
      primary: "New document",
      hint: "prose, lists, todos",
      run: onNewZenDocument,
    },
    {
      key: "action-library",
      group: "Actions",
      primary: "New library",
      hint: "create and open",
      run: onNewLibrary,
    },
  ];

  function moveSelection(offset: number) {
    setActiveIndex((current) => (current + offset + options.length) % options.length);
  }

  useEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog) return;
    dialog.showModal();
    dialog.querySelector<HTMLInputElement>("input")?.focus();
    return () => {
      if (dialog.open) dialog.close();
    };
  }, []);

  useEffect(() => {
    document
      .getElementById(`command-option-${activeIndex}`)
      ?.scrollIntoView({ block: "nearest" });
  }, [activeIndex]);

  return (
    <dialog
      aria-label="Search or run a command"
      className="command-dialog"
      onCancel={(event) => {
        event.preventDefault();
        onClose();
      }}
      ref={dialogRef}
    >
      <div className="command-input">
        <span aria-hidden="true">&gt;</span>
        <input
          aria-activedescendant={`command-option-${activeIndex}`}
          aria-controls="command-results"
          aria-expanded="true"
          aria-label="Search saves, documents, and commands"
          onChange={(event) => {
            setQuery(event.target.value);
            setActiveIndex(0);
          }}
          onKeyDown={(event) => {
            if (
              (event.ctrlKey && !event.altKey && !event.metaKey && !event.shiftKey &&
                event.key.toLocaleLowerCase() === "n") ||
              event.key === "ArrowDown"
            ) {
              event.preventDefault();
              moveSelection(1);
            } else if (
              (event.ctrlKey && !event.altKey && !event.metaKey && !event.shiftKey &&
                event.key.toLocaleLowerCase() === "p") ||
              event.key === "ArrowUp"
            ) {
              event.preventDefault();
              moveSelection(-1);
            } else if (event.key === "Enter" && !event.nativeEvent.isComposing) {
              event.preventDefault();
              options[activeIndex]?.run();
            }
          }}
          placeholder="Search saves, documents, and commands"
          role="combobox"
          value={query}
        />
      </div>
      <div className="command-results" id="command-results" role="listbox">
        {options.map((option, index) => (
          <Fragment key={option.key}>
            {index === 0 || options[index - 1]!.group !== option.group ? (
              <p role="presentation">{option.group}</p>
            ) : null}
            <button
              aria-selected={activeIndex === index}
              className={activeIndex === index ? "command-selected" : undefined}
              id={`command-option-${index}`}
              onClick={option.run}
              onPointerEnter={() => setActiveIndex(index)}
              role="option"
              type="button"
            >
              <span>{option.primary}</span>
              {option.detail ? <small>{option.detail}</small> : null}
              {option.hint ? <em>{option.hint}</em> : null}
            </button>
          </Fragment>
        ))}
      </div>
      <footer className="command-footer"><span><b>Ctrl P/N · ↑↓</b> navigate</span><span><b>↵</b> select</span><span><b>esc</b> close</span></footer>
    </dialog>
  );
}

function ReaderView({
  busy,
  item,
  onBack,
  onDelete,
  onEdit,
  onFavorite,
}: {
  busy: boolean;
  item: LibraryItemView;
  onBack: () => void;
  onDelete: (item: LibraryItemView) => Promise<void>;
  onEdit: (item: LibraryItemView, opener: HTMLButtonElement) => void;
  onFavorite: (item: LibraryItemView) => Promise<void>;
}) {
  const [itemsWithImages, setItemsWithImages] = useState<Set<string>>(
    () => new Set(),
  );
  const [expandedImage, setExpandedImage] = useState<{
    image: MarkdownImage;
    opener: HTMLButtonElement;
  } | null>(null);
  const [capturedContext, setCapturedContext] = useState<string | null>(null);
  const [capturedContextError, setCapturedContextError] = useState<string | null>(
    null,
  );
  const [capturedContextLoading, setCapturedContextLoading] = useState(false);
  const imageCloseRef = useRef<HTMLButtonElement | null>(null);
  const label = item.title?.trim() || item.url;
  const context = capturedContext ?? item.excerpt;
  const hasContext = Boolean(context?.trim());
  const imagesAllowed = itemsWithImages.has(item.id);

  useEffect(() => {
    setExpandedImage(null);
  }, [item.id]);

  useEffect(() => {
    let cancelled = false;
    setCapturedContext(null);
    setCapturedContextError(null);
    const reference = item.capturedDocument;
    if (!reference) {
      setCapturedContextLoading(false);
      return () => {
        cancelled = true;
      };
    }
    setCapturedContextLoading(true);
    void browserSync.loadCapturedDocument(reference).then(
      (markdown) => {
        if (cancelled) return;
        setCapturedContext(markdown);
        setCapturedContextLoading(false);
      },
      (error: unknown) => {
        if (cancelled) return;
        setCapturedContextError(
          error instanceof Error
            ? error.message
            : "Captured page content could not be loaded.",
        );
        setCapturedContextLoading(false);
      },
    );
    return () => {
      cancelled = true;
    };
  }, [item.id, item.capturedDocument?.sha256]);

  useEffect(() => {
    if (expandedImage) imageCloseRef.current?.focus();
  }, [expandedImage]);

  function loadImagesForItem() {
    setItemsWithImages((current) => {
      const next = new Set(current);
      next.add(item.id);
      return next;
    });
  }

  function closeExpandedImage() {
    const opener = expandedImage?.opener;
    const imageSrc = expandedImage?.image.src;
    setExpandedImage(null);
    window.requestAnimationFrame(() => {
      if (opener?.isConnected) {
        opener.focus();
        return;
      }
      const matchingTrigger = [
        ...document.querySelectorAll<HTMLButtonElement>(
          ".reader-markdown-image-trigger",
        ),
      ].find((candidate) => candidate.dataset.imageSrc === imageSrc);
      matchingTrigger?.focus();
    });
  }

  return (
    <section className="reader-view" aria-labelledby="reader-title">
      <article className="reader-article">
        <header className="reader-toolbar">
          <button className="reader-back" onClick={onBack} type="button">
            <span aria-hidden="true">←</span>
            <span>All saves</span>
          </button>
          <div className="reader-toolbar-actions">
            <button
              aria-label={item.favorite ? "Remove favorite" : "Favorite"}
              aria-pressed={item.favorite}
              className="icon-button"
              disabled={busy}
              onClick={() => void onFavorite(item)}
              type="button"
            >
              <span aria-hidden="true">★</span>
            </button>
            <button
              aria-haspopup="dialog"
              aria-label="Edit save"
              className="icon-button"
              disabled={busy}
              onClick={(event) => onEdit(item, event.currentTarget)}
              type="button"
            >
              <span aria-hidden="true">✎</span>
            </button>
            <a
              aria-label="Open original"
              className="icon-button"
              href={item.url}
              rel="noreferrer"
              target="_blank"
            >
              <span aria-hidden="true">↗</span>
            </a>
            <button
              aria-label="Archive"
              className="icon-button danger-button"
              disabled={busy}
              onClick={() => void onDelete(item).then(onBack)}
              type="button"
            >
              <span aria-hidden="true">⌫</span>
            </button>
          </div>
        </header>
        <div className="reader-content">
          <div className="reader-meta">
            <span>{readHostname(item.url)} · Saved {formatDate(item.savedAt)}</span>
          </div>
          <h1 id="reader-title">{label}</h1>
          {item.tags.length > 0 ? <p className="reader-tags">{item.tags.map((tag) => <span key={tag}>#{tag}</span>)}</p> : null}
          {item.note?.trim() ? <aside className="reader-note"><strong>Your note</strong><p>{item.note}</p></aside> : null}
          <div className="reader-body">
            {capturedContextLoading ? (
              <p role="status">Loading captured page content…</p>
            ) : null}
            {capturedContextError ? (
              <p className="sync-error" role="alert">{capturedContextError}</p>
            ) : null}
            {hasContext && context ? (
              <MarkdownDocument
                imageBaseUrl={item.url}
                imagesAllowed={imagesAllowed}
                onLoadImages={loadImagesForItem}
                onOpenImage={(image, opener) =>
                  setExpandedImage({ image, opener })
                }
                source={context}
              />
            ) : capturedContextLoading ? null : (
              <p>This save has no extracted preview yet. Open the original to read the full page, or add a private note to keep the context that matters.</p>
            )}
            <p className="reader-source">ResearchPocket keeps the URL and your authored context locally. The original page remains at <a href={item.url} rel="noreferrer" target="_blank">{readHostname(item.url)}</a>.</p>
          </div>
        </div>
      </article>
      {expandedImage ? (
        <div
          aria-label={expandedImage.image.alt || "Expanded image"}
          aria-modal="true"
          className="reader-image-viewer"
          onClick={(event) => {
            if (event.target === event.currentTarget) closeExpandedImage();
          }}
          onKeyDown={(event) => {
            if (event.key === "Escape") {
              event.preventDefault();
              event.stopPropagation();
              closeExpandedImage();
            } else if (event.key === "Tab") {
              event.preventDefault();
              imageCloseRef.current?.focus();
            }
          }}
          role="dialog"
        >
          <button
            aria-label="Close expanded image"
            className="reader-image-viewer-close"
            onClick={closeExpandedImage}
            ref={imageCloseRef}
            type="button"
          >
            ×
          </button>
          <figure className="reader-image-viewer-stage">
            <img
              alt={expandedImage.image.alt}
              referrerPolicy="no-referrer"
              src={expandedImage.image.src}
            />
            {expandedImage.image.title ? (
              <figcaption>{expandedImage.image.title}</figcaption>
            ) : null}
          </figure>
        </div>
      ) : null}
    </section>
  );
}

function CaptureForm({
  busy,
  error,
  knownTags,
  onAdd,
}: {
  busy: boolean;
  error: string | null;
  knownTags: string[];
  onAdd: (input: AddInput) => Promise<boolean>;
}) {
  const [url, setUrl] = useState("");
  const [title, setTitle] = useState("");
  const [tags, setTags] = useState<string[]>([]);
  const [tagDraft, setTagDraft] = useState("");
  const [note, setNote] = useState("");
  const [favorite, setFavorite] = useState(false);

  async function submit(event: SubmitEvent<HTMLFormElement>) {
    event.preventDefault();
    const saved = await onAdd({
      favorite,
      note: optionalText(note),
      tags: withTagDraft(tags, tagDraft),
      title: optionalText(title),
      url: url.trim(),
    } as AddInput);

    if (saved) {
      setUrl("");
      setTitle("");
      setTags([]);
      setTagDraft("");
      setNote("");
      setFavorite(false);
    }
  }

  return (
    <form className="capture-form" onSubmit={(event) => void submit(event)}>
      <label className="field field-url">
        <span>URL</span>
        <input
          autoCapitalize="none"
          autoComplete="url"
          id="capture-url"
          inputMode="url"
          name="url"
          onChange={(event) => setUrl(event.target.value)}
          placeholder="https://example.com/a-useful-page"
          required
          type="url"
          value={url}
        />
      </label>

      <label className="field">
        <span>Title <small>optional</small></span>
        <input
          autoComplete="off"
          id="capture-title"
          name="title"
          onChange={(event) => setTitle(event.target.value)}
          placeholder="What will help you recognize it?"
          type="text"
          value={title}
        />
      </label>

      <TagField
        id="capture-tags"
        knownTags={knownTags}
        onDraftChange={setTagDraft}
        onChange={setTags}
        draft={tagDraft}
        value={tags}
      />

      <label className="field field-note">
        <span>Private note <small>optional</small></span>
        <textarea
          id="capture-note"
          name="note"
          onChange={(event) => setNote(event.target.value)}
          placeholder="Why are you keeping this?"
          rows={3}
          value={note}
        />
      </label>

      <div className="capture-actions">
        <label className="check-field">
          <input
            checked={favorite}
            id="capture-favorite"
            name="favorite"
            onChange={(event) => setFavorite(event.target.checked)}
            type="checkbox"
          />
          <span>Mark as a favorite</span>
        </label>
        <button className="primary-button" disabled={busy} type="submit">
          {busy ? "Saving…" : "Save to library"}
        </button>
      </div>
      {error ? <p className="dialog-error" role="alert">{error}</p> : null}
    </form>
  );
}

function CaptureDialog({
  busy,
  error,
  knownTags,
  onAdd,
  onClose,
}: {
  busy: boolean;
  error: string | null;
  knownTags: string[];
  onAdd: (input: AddInput) => Promise<boolean>;
  onClose: () => void;
}) {
  const dialogRef = useRef<HTMLDialogElement>(null);

  useEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog) return;

    dialog.showModal();
    dialog.querySelector<HTMLInputElement>("#capture-url")?.focus();
    return () => {
      if (dialog.open) dialog.close();
    };
  }, []);

  return (
    <dialog
      aria-labelledby="capture-dialog-heading"
      className="capture-dialog"
      onCancel={(event) => {
        event.preventDefault();
        onClose();
      }}
      ref={dialogRef}
    >
      <div className="dialog-surface">
        <div className="dialog-heading">
          <div>
            <h2 id="capture-dialog-heading">New save</h2>
            <p>Committed locally before synchronization.</p>
          </div>
          <button
            aria-label="Close capture form"
            className="icon-button close-button"
            disabled={busy}
            onClick={onClose}
            title="Close"
            type="button"
          >
            <span aria-hidden="true">×</span>
          </button>
        </div>
        <CaptureForm
          busy={busy}
          error={error}
          knownTags={knownTags}
          onAdd={onAdd}
        />
      </div>
    </dialog>
  );
}

function TagField({
  draft,
  id,
  knownTags,
  onDraftChange,
  onChange,
  value,
}: {
  draft: string;
  id: string;
  knownTags: string[];
  onDraftChange: (value: string) => void;
  onChange: (value: string[]) => void;
  value: string[];
}) {
  const suggestions = findTagSuggestions(knownTags, value, draft);
  const suggestionsId = `${id}-suggestions`;

  function addTag(tag: string) {
    const trimmed = tag.trim();
    if (trimmed && !value.includes(trimmed)) onChange([...value, trimmed]);
    onDraftChange("");
  }

  return (
    <div className="field tag-field">
      <label className="field-caption" htmlFor={id}>
        Tags <small>Enter or comma to add</small>
      </label>
      <input
        aria-autocomplete="list"
        aria-controls={suggestions.length > 0 ? suggestionsId : undefined}
        aria-expanded={suggestions.length > 0}
        autoComplete="off"
        id={id}
        name="tag-input"
        onBlur={() => {
          if (draft.trim()) addTag(draft);
        }}
        onChange={(event) => onDraftChange(event.target.value)}
        onKeyDown={(event) => {
          if (event.key === "Enter" || event.key === ",") {
            event.preventDefault();
            addTag(draft);
          } else if (event.key === "Backspace" && draft === "" && value.length > 0) {
            onChange(value.slice(0, -1));
          }
        }}
        placeholder={value.length > 0 ? "Add another tag" : "Add tags"}
        type="text"
        value={draft}
      />
      {value.length > 0 ? (
        <div aria-label="Selected tags" className="selected-tags">
          {value.map((tag) => (
            <span key={tag}>
              {tag}
              <button
                aria-label={`Remove tag ${tag}`}
                onClick={() => onChange(value.filter((value) => value !== tag))}
                type="button"
              >
                ×
              </button>
            </span>
          ))}
        </div>
      ) : null}
      {suggestions.length > 0 ? (
        <div aria-label="Tag suggestions" className="tag-suggestions" id={suggestionsId}>
          {suggestions.map((tag) => (
            <button
              key={tag}
              onMouseDown={(event) => event.preventDefault()}
              onClick={() => addTag(tag)}
              type="button"
            >
              + {tag}
            </button>
          ))}
        </div>
      ) : null}
    </div>
  );
}

function LibraryItem({
  busy,
  item,
  onDelete,
  onEdit,
  onFavorite,
  onRead,
  onRestore,
  onToggleTagFilter,
  selectedTags,
}: {
  busy: boolean;
  item: LibraryItemView;
  onDelete: (item: LibraryItemView) => Promise<void>;
  onEdit: (item: LibraryItemView, opener: HTMLButtonElement) => void;
  onFavorite: (item: LibraryItemView) => Promise<void>;
  onRead: (item: LibraryItemView) => void;
  onRestore: (item: LibraryItemView) => Promise<void>;
  onToggleTagFilter: (tag: string) => void;
  selectedTags: string[];
}) {
  const label = item.title?.trim() || item.url;

  return (
    <li>
      <article
        className={`item-card${
          item.deleted ? " item-card-deleted" : " item-card-editable"
        }${item.favorite ? " item-card-favorite" : ""}`}
      >
        {!item.deleted ? (
          <button
            aria-label={`Read ${label}${item.favorite ? ", favorite" : ""}`}
            className="item-card-edit-trigger"
            disabled={busy}
            onClick={() => onRead(item)}
            title="Open reader"
            type="button"
          />
        ) : null}
        <div className="item-row-copy">
          <h3>
            {/* The star reads as a mark on the title rather than a column of
                its own, which is what let the row lose a whole grid track. */}
            {item.favorite ? (
              <span aria-hidden="true" className="item-star">
                ★
              </span>
            ) : null}
            {item.deleted ? (
              <a href={item.url} rel="noreferrer" target="_blank">
                {label}
                <span className="sr-only"> (opens in a new tab)</span>
              </a>
            ) : (
              <span>
                {label}
                {item.favorite ? <span className="sr-only">, favorite</span> : null}
              </span>
            )}
          </h3>
          <p className="item-meta">
            <span>{readHostname(item.url)}</span>
            <span aria-hidden="true">·</span>
            <span>{item.deleted ? "Deleted" : "Saved"} {formatDate(item.savedAt)}</span>
            {item.tags.length > 0 ? (
              <span
                aria-label={`Tags for ${label}`}
                className="item-inline-tags"
                role="group"
              >
                {item.tags.map((tag) => {
                  const selected = selectedTags.includes(tag);
                  return (
                    <button
                      aria-label={`#${tag}, ${
                        selected ? "remove" : "add"
                      } tag filter`}
                      aria-pressed={selected}
                      className="item-inline-tag"
                      key={tag}
                      onClick={() => onToggleTagFilter(tag)}
                      type="button"
                    >
                      #{tag}
                    </button>
                  );
                })}
              </span>
            ) : null}
          </p>
        </div>

        {/* Two controls carry the row: the one thing that leaves the app, and
            everything else. Reader is the row itself, so it is not repeated
            here — it stays in the menu for the keyboard. */}
        <div aria-label={`Actions for ${label}`} className="item-row-actions" role="group">
          <a
            aria-label={`Open ${label} in a new tab`}
            className="icon-button"
            href={item.url}
            rel="noreferrer"
            target="_blank"
            title="Open in new tab"
          >
            <span aria-hidden="true">↗</span>
          </a>
          <ItemMenu
            busy={busy}
            item={item}
            label={label}
            onDelete={onDelete}
            onEdit={onEdit}
            onFavorite={onFavorite}
            onRead={onRead}
            onRestore={onRestore}
          />
        </div>
      </article>
    </li>
  );
}

/**
 * The row's second control. Everything that used to sit in a four-icon strip
 * lives here, which is what let the resting row show two.
 */
function ItemMenu({
  busy,
  item,
  label,
  onDelete,
  onEdit,
  onFavorite,
  onRead,
  onRestore,
}: {
  busy: boolean;
  item: LibraryItemView;
  label: string;
  onDelete: (item: LibraryItemView) => Promise<void>;
  onEdit: (item: LibraryItemView, opener: HTMLButtonElement) => void;
  onFavorite: (item: LibraryItemView) => Promise<void>;
  onRead: (item: LibraryItemView) => void;
  onRestore: (item: LibraryItemView) => Promise<void>;
}) {
  // Rows carry paint containment so a long list stays cheap to scroll, which
  // no absolutely positioned child can escape. The menu is placed against the
  // viewport instead, and closes on anything that would move it.
  const [anchor, setAnchor] = useState<{ right: number; top: number } | null>(
    null,
  );
  const open = anchor !== null;
  const trigger = useRef<HTMLButtonElement>(null);
  const wrapper = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    function close() {
      setAnchor(null);
    }
    function handlePointer(event: MouseEvent) {
      const target = event.target as Node;
      if (wrapper.current?.contains(target)) return;
      if ((target as HTMLElement).closest?.(".item-menu-list")) return;
      close();
    }
    function handleKey(event: KeyboardEvent) {
      if (event.key !== "Escape") return;
      // Stopped here so the same key does not also close the reader behind it.
      event.stopPropagation();
      close();
      trigger.current?.focus();
    }
    window.addEventListener("pointerdown", handlePointer);
    window.addEventListener("keydown", handleKey, true);
    window.addEventListener("resize", close);
    window.addEventListener("scroll", close, true);
    return () => {
      window.removeEventListener("pointerdown", handlePointer);
      window.removeEventListener("keydown", handleKey, true);
      window.removeEventListener("resize", close);
      window.removeEventListener("scroll", close, true);
    };
  }, [open]);

  function run(action: () => void) {
    setAnchor(null);
    action();
  }

  function toggle() {
    if (open) {
      setAnchor(null);
      return;
    }
    const rect = trigger.current?.getBoundingClientRect();
    if (!rect) return;
    // Rows near the foot of the window have no room beneath them.
    const dropUp = rect.bottom > window.innerHeight - 12 * 16;
    setAnchor({
      right: Math.max(8, window.innerWidth - rect.right),
      top: dropUp ? rect.top - 4 : rect.bottom + 4,
    });
  }

  return (
    <div className="item-menu" ref={wrapper}>
      <button
        aria-expanded={open}
        aria-haspopup="menu"
        aria-label={`More actions for ${label}`}
        className="icon-button"
        disabled={busy}
        onClick={toggle}
        ref={trigger}
        title="More"
        type="button"
      >
        <span aria-hidden="true">⋯</span>
      </button>
      {anchor ? createPortal(
        <div
          className="item-menu-list"
          role="menu"
          style={{
            insetInlineEnd: `${anchor.right}px`,
            top: `${anchor.top}px`,
            transform:
              anchor.top < window.innerHeight / 2 ? undefined : "translateY(-100%)",
          }}
        >
          {item.deleted ? (
            <button
              onClick={() => run(() => void onRestore(item))}
              role="menuitem"
              type="button"
            >
              <span aria-hidden="true">↶</span> Restore
            </button>
          ) : (
            <>
              <button
                onClick={() => run(() => onRead(item))}
                role="menuitem"
                type="button"
              >
                <span aria-hidden="true">¶</span> Reader
              </button>
              <button
                onClick={() => run(() => void onFavorite(item))}
                role="menuitem"
                type="button"
              >
                <span aria-hidden="true">★</span>{" "}
                {item.favorite ? "Remove favorite" : "Favorite"}
              </button>
              <button
                onClick={(event) => {
                  const opener = trigger.current ?? event.currentTarget;
                  run(() => onEdit(item, opener));
                }}
                role="menuitem"
                type="button"
              >
                <span aria-hidden="true">✎</span> Edit details
              </button>
              <button
                className="item-menu-danger"
                onClick={() => run(() => void onDelete(item))}
                role="menuitem"
                type="button"
              >
                <span aria-hidden="true">⌫</span> Delete
              </button>
            </>
          )}
        </div>,
        // Inside the shell, not the body: every control in this app takes its
        // press feel from `.app-shell`, and a menu that escapes containment
        // must not escape that too.
        document.querySelector(".app-shell") ?? document.body,
      ) : null}
    </div>
  );
}

function EditDialog({
  busy,
  error,
  item,
  knownTags,
  onClose,
  onSave,
}: {
  busy: boolean;
  error: string | null;
  item: LibraryItemView;
  knownTags: string[];
  onClose: () => void;
  onSave: (item: LibraryItemView, input: EditInput) => Promise<boolean>;
}) {
  const dialogRef = useRef<HTMLDialogElement>(null);
  const titleRef = useRef<HTMLInputElement>(null);
  const [url, setUrl] = useState(item.url);
  const [title, setTitle] = useState(item.title ?? "");
  const [excerpt, setExcerpt] = useState(item.excerpt ?? "");
  const [tags, setTags] = useState(item.tags);
  const [tagDraft, setTagDraft] = useState("");
  const [note, setNote] = useState(item.note ?? "");
  const [favorite, setFavorite] = useState(item.favorite);

  useEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog) {
      return;
    }

    dialog.showModal();
    titleRef.current?.focus();

    return () => {
      if (dialog.open) {
        dialog.close();
      }
    };
  }, []);

  async function submit(event: SubmitEvent<HTMLFormElement>) {
    event.preventDefault();
    await onSave(item, {
      excerpt: optionalText(excerpt),
      favorite,
      note: optionalText(note),
      tags: withTagDraft(tags, tagDraft),
      title: optionalText(title),
      url: url.trim(),
    } as EditInput);
  }

  return (
    <dialog
      aria-labelledby="edit-heading"
      className="edit-dialog"
      onCancel={(event) => {
        event.preventDefault();
        onClose();
      }}
      ref={dialogRef}
    >
      <form className="edit-form" onSubmit={(event) => void submit(event)}>
        <div className="dialog-heading">
          <div>
            <h2 id="edit-heading">Edit save</h2>
            <p>{readHostname(item.url)}</p>
          </div>
          <button
            aria-label="Close edit form"
            className="icon-button close-button"
            disabled={busy}
            onClick={onClose}
            title="Close"
            type="button"
          >
            <span aria-hidden="true">×</span>
          </button>
        </div>

        <div className="dialog-fields">
          <label className="field">
            <span>URL</span>
            <input
              autoCapitalize="none"
              id="edit-url"
              name="url"
              onChange={(event) => setUrl(event.target.value)}
              required
              type="url"
              value={url}
            />
          </label>
          <label className="field">
            <span>Title</span>
            <input
              id="edit-title"
              name="title"
              onChange={(event) => setTitle(event.target.value)}
              ref={titleRef}
              type="text"
              value={title}
            />
          </label>
          <label className="field">
            <span>Excerpt</span>
            <textarea
              id="edit-excerpt"
              name="excerpt"
              onChange={(event) => setExcerpt(event.target.value)}
              rows={3}
              value={excerpt}
            />
          </label>
          <TagField
            id="edit-tags"
            knownTags={knownTags}
            onDraftChange={setTagDraft}
            onChange={setTags}
            draft={tagDraft}
            value={tags}
          />
          <label className="field">
            <span>Private note</span>
            <textarea
              id="edit-note"
              name="note"
              onChange={(event) => setNote(event.target.value)}
              rows={5}
              value={note}
            />
          </label>
          <label className="check-field">
            <input
              checked={favorite}
              id="edit-favorite"
              name="favorite"
              onChange={(event) => setFavorite(event.target.checked)}
              type="checkbox"
            />
            <span>Favorite</span>
          </label>
          {error ? <p className="dialog-error" role="alert">{error}</p> : null}
        </div>

        <div className="dialog-actions">
          <button
            className="secondary-button"
            disabled={busy}
            onClick={onClose}
            type="button"
          >
            Cancel
          </button>
          <button className="primary-button" disabled={busy} type="submit">
            {busy ? "Saving changes…" : "Save changes"}
          </button>
        </div>
      </form>
    </dialog>
  );
}

function EmptyLibrary({
  filter,
  hasQuery,
}: {
  filter: LifecycleFilter;
  hasQuery: boolean;
}) {
  let heading = "Your library is ready.";
  let body: ReactNode = (
    <>
      Choose New save to keep your first link. A title or note can help your
      future self remember why it mattered.
    </>
  );

  if (hasQuery) {
    heading = "No saves match that search.";
    body = "Try fewer words, another tag, or clear the search field.";
  } else if (filter === "deleted") {
    heading = "Nothing is waiting for recovery.";
    body = "Deleted saves stay here so an accidental click is easy to undo.";
  }

  return (
    <div className="empty-state">
      <p aria-hidden="true" className="empty-mark">
        {filter === "deleted" ? "↶" : "+"}
      </p>
      <h3>{heading}</h3>
      <p>{body}</p>
    </div>
  );
}

function filterAndSortItems(
  items: LibraryItemView[],
  query: string,
  filter: LifecycleFilter,
  searchScope: SearchScope,
  favoriteOnly: boolean,
  selectedTags: string[],
  tagMatchMode: TagMatchMode,
  sortMode: SortMode,
) {
  const normalizedQuery = query.trim().toLocaleLowerCase();

  return items
    .filter(
      (item) =>
        filter === "all" || item.deleted === (filter === "deleted"),
    )
    .filter((item) => !favoriteOnly || item.favorite)
    .filter((item) => {
      if (selectedTags.length === 0) return true;
      return tagMatchMode === "all"
        ? selectedTags.every((tag) => item.tags.includes(tag))
        : selectedTags.some((tag) => item.tags.includes(tag));
    })
    .filter((item) => {
      if (!normalizedQuery) {
        return true;
      }

      const valuesByScope: Record<SearchScope, Array<string | null | undefined>> = {
        all: [item.url, item.title, item.excerpt, item.note, ...item.tags],
        context: [item.excerpt, item.note],
        tags: item.tags,
        title: [item.title],
        url: [item.url],
      };

      return valuesByScope[searchScope]
        .filter((value): value is string => typeof value === "string")
        .some((value) => value.toLocaleLowerCase().includes(normalizedQuery));
    })
    .map((item) => ({
      item,
      label: (item.title?.trim() || item.url).toLocaleLowerCase(),
      savedTime: Date.parse(item.savedAt) || 0,
    }))
    .sort((left, right) => {
      if (sortMode === "title") return left.label.localeCompare(right.label);
      if (sortMode === "oldest") return left.savedTime - right.savedTime;

      return right.savedTime - left.savedTime;
    })
    .map(({ item }) => item);
}

function findTagSuggestions(knownTags: string[], selectedTags: string[], draft: string) {
  const fragment = draft.trim().toLocaleLowerCase();
  if (!fragment) return [];
  const selected = new Set(selectedTags.map((tag) => tag.toLocaleLowerCase()));
  return knownTags
    .filter((tag) => !selected.has(tag.toLocaleLowerCase()))
    .filter((tag) => fragment === "" || tag.toLocaleLowerCase().includes(fragment))
    .slice(0, 5);
}

function withTagDraft(tags: string[], draft: string) {
  const trimmedDraft = draft.trim();
  return Array.from(new Set(trimmedDraft ? [...tags, trimmedDraft] : tags));
}

function readSyncCredential(form: HTMLFormElement) {
  const data = new FormData(form);
  const token = data.get("github-token");
  if (typeof token !== "string") {
    throw new Error("Enter a fine-grained GitHub token.");
  }
  return {
    token,
    rememberForTab: data.get("remember-for-tab") === "on",
  };
}

function clearCredentialInput(form: HTMLFormElement) {
  const input = form.elements.namedItem("github-token");
  if (input instanceof HTMLInputElement) input.value = "";
}

function optionalText(value: string) {
  return value.length > 0 ? value : null;
}

function readHostname(value: string) {
  try {
    return new URL(value).hostname.replace(/^www\./, "");
  } catch {
    return "saved link";
  }
}

function pendingChangeAction(kind: PendingSyncChange["kind"]) {
  switch (kind) {
    case "create":
      return "Added";
    case "edit":
      return "Edited";
    case "delete":
      return "Deleted";
    case "restore":
      return "Restored";
    default:
      return "Queued";
  }
}

function pendingChangeDetails(change: PendingSyncChange) {
  if (change.kind === "queued") {
    return "Stored before detailed change labels were available.";
  }
  if (change.kind === "delete") {
    return "Will move this save to deleted items.";
  }
  if (change.kind === "restore") {
    return "Will return this save to active items.";
  }

  const details: string[] = [];
  if (change.fields.length > 0) {
    const fields = change.fields.map(pendingFieldLabel).join(", ");
    details.push(change.kind === "create" ? `Includes ${fields}` : `Changed ${fields}`);
  }
  if (change.favorite !== null) {
    details.push(
      change.favorite
        ? change.kind === "create"
          ? "Favorite"
          : "Added to favorites"
        : "Removed from favorites",
    );
  }
  if (change.addedTags.length > 0) {
    const tags = change.addedTags.map((tag) => `#${tag}`).join(", ");
    details.push(change.kind === "create" ? `Tags ${tags}` : `Added ${tags}`);
  }
  if (change.removedTags.length > 0) {
    details.push(`Removed ${change.removedTags.map((tag) => `#${tag}`).join(", ")}`);
  }
  return details.join(" · ") || "Local library edit.";
}

function pendingFieldLabel(field: PendingSyncChange["fields"][number]) {
  switch (field) {
    case "url":
      return "URL";
    case "note":
      return "private note";
    default:
      return field;
  }
}

function formatDate(value: string) {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) {
    return "recently";
  }

  return new Intl.DateTimeFormat(undefined, {
    dateStyle: "medium",
  }).format(date);
}

function formatDateTime(value: string) {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) {
    return "Recently";
  }
  return new Intl.DateTimeFormat(undefined, {
    dateStyle: "medium",
    timeStyle: "short",
  }).format(date);
}

function pluralize(count: number, noun: string) {
  return `${count.toLocaleString()} ${noun}${count === 1 ? "" : "s"}`;
}

function readWorkspaceView(): WorkspaceView {
  const hash = window.location.hash;
  if (hash === "#sync" || hash === "#restore") return "sync";
  if (hash === "#settings") return "settings";
  // The reader is a library surface, so a deep-linked item closes back into the
  // library rather than into the default workspace.
  if (hash === "#library" || hash.startsWith("#item=")) return "library";
  return "zen";
}

/**
 * Zen is the default workspace, so it is the one view that owns the bare URL.
 */
function hashForView(target: WorkspaceView) {
  return target === "zen" ? "" : `#${target}`;
}

/** Builds a same-document URL, keeping an empty query out of the address bar. */
function formatUrl(params: URLSearchParams, hash: string) {
  const search = params.toString();
  return `${window.location.pathname}${search ? `?${search}` : ""}${hash}`;
}

/** The URL decides, and it always carries the mode once the app has opened. */
function readLibraryModeFromUrl(): LibraryMode {
  return new URLSearchParams(window.location.search).get("mode") === "graph"
    ? "graph"
    : "list";
}

/** Opening reading only: a shared link wins over the remembered preference. */
function readInitialLibraryMode(): LibraryMode {
  const parameter = new URLSearchParams(window.location.search).get("mode");
  if (parameter === "graph") return "graph";
  if (parameter === "list") return "list";
  try {
    return window.localStorage.getItem(LIBRARY_MODE_STORAGE_KEY) === "graph"
      ? "graph"
      : "list";
  } catch {
    return "list";
  }
}

function readDensityPreference(): Density {
  try {
    return window.localStorage.getItem(DENSITY_STORAGE_KEY) === "compact"
      ? "compact"
      : "comfortable";
  } catch {
    return "comfortable";
  }
}

function readRailCollapsedPreference(): boolean {
  try {
    return window.localStorage.getItem(RAIL_COLLAPSED_STORAGE_KEY) === "true";
  } catch {
    return false;
  }
}

function readThemePreference(): ThemeColors {
  try {
    const stored = JSON.parse(window.localStorage.getItem(THEME_STORAGE_KEY) ?? "null");
    if (stored?.version === 1 && isThemeColors(stored.colors)) {
      return stored.colors;
    }
  } catch {
    // Invalid or unavailable storage falls back to the shipped theme.
  }
  return { ...DEFAULT_THEME };
}

function readImageBackgroundPreference() {
  try {
    const stored = window.localStorage.getItem(IMAGE_BACKGROUND_STORAGE_KEY);
    return stored && /^#[0-9a-f]{6}$/i.test(stored)
      ? stored
      : DEFAULT_IMAGE_BACKGROUND;
  } catch {
    return DEFAULT_IMAGE_BACKGROUND;
  }
}

function readReaderMeasurePreference(): ReaderMeasure {
  try {
    const stored = window.localStorage.getItem(READER_MEASURE_STORAGE_KEY);
    const match = READER_MEASURES.find(({ id }) => id === stored);
    if (match) return match.id;
  } catch {
    // Invalid or unavailable storage falls back to the shipped measure.
  }
  return DEFAULT_READER_MEASURE;
}

function isThemeColors(value: unknown): value is ThemeColors {
  if (!value || typeof value !== "object") return false;
  const colors = value as Record<string, unknown>;
  return THEME_FIELDS.every(
    ({ key }) => typeof colors[key] === "string" && /^#[0-9a-f]{6}$/i.test(colors[key]),
  );
}

function themesEqual(left: ThemeColors, right: ThemeColors) {
  return THEME_FIELDS.every(({ key }) => left[key] === right[key]);
}

function applyThemePreference(theme: ThemeColors) {
  const root = document.documentElement;
  const derivedProperties = {
    "--color-surface": "color-mix(in srgb, var(--background) 92%, var(--text))",
    "--color-surface-raised": "color-mix(in srgb, var(--background) 89%, var(--text))",
    "--color-surface-muted": "color-mix(in srgb, var(--background) 86%, var(--text))",
    "--color-text-muted": "color-mix(in srgb, var(--text) 55%, var(--background))",
    "--color-text-soft": "color-mix(in srgb, var(--text) 72%, var(--background))",
    "--color-text-reader": "color-mix(in srgb, var(--text) 88%, var(--background))",
    "--color-favorite-muted": "color-mix(in srgb, var(--text) 34%, var(--background))",
    "--color-border": "color-mix(in srgb, var(--text) 20%, var(--background))",
    "--color-border-subtle": "color-mix(in srgb, var(--text) 9%, var(--background))",
    "--color-border-strong": "color-mix(in srgb, var(--text) 38%, var(--background))",
    "--color-accent-soft": "color-mix(in srgb, var(--secondary) 28%, var(--background))",
    "--color-reader-note": "color-mix(in srgb, var(--background) 94%, var(--text))",
  };
  const custom = !themesEqual(theme, DEFAULT_THEME);

  for (const { key } of THEME_FIELDS) {
    if (custom) root.style.setProperty(`--${key}`, theme[key]);
    else root.style.removeProperty(`--${key}`);
  }
  for (const [property, value] of Object.entries(derivedProperties)) {
    if (custom) root.style.setProperty(property, value);
    else root.style.removeProperty(property);
  }

  if (custom) root.style.colorScheme = prefersLightControls(theme.background) ? "light" : "dark";
  else root.style.removeProperty("color-scheme");
}

function prefersLightControls(color: string) {
  const red = Number.parseInt(color.slice(1, 3), 16);
  const green = Number.parseInt(color.slice(3, 5), 16);
  const blue = Number.parseInt(color.slice(5, 7), 16);
  return red * 0.299 + green * 0.587 + blue * 0.114 > 160;
}

function readError(error: unknown) {
  if (error instanceof Error) {
    return error.message;
  }
  return typeof error === "string" ? error : "The local library could not finish that action.";
}

function readStateError(error: unknown) {
  if (error === null || error === undefined || error === "") {
    return null;
  }
  return readError(error);
}

function formatViewLabel(view: WorkspaceView) {
  if (view === "library") return "Library";
  if (view === "zen") return "Zen";
  if (view === "sync") return "Sync";
  return "Settings";
}

/**
 * What the chip shows. The arrow already says the count is waiting to leave
 * this device, so the word "pending" beside it is saying it twice — it stays
 * only in the accessible name, where there is no arrow to read.
 */
function formatSyncChip(status: unknown, pendingCount: number) {
  if (pendingCount > 0) return `↑ ${pendingCount.toLocaleString()}`;
  if (status === "error") return "! attention";
  if (status === "saving") return "· saving";
  if (status === "loading" || status === "opening" || status === "initializing") {
    return "· opening";
  }
  return "✓ synced";
}

/** The wide-window reading, which has room for a word but not a repeated one. */
function formatSyncCopy(status: unknown, pendingCount: number) {
  if (pendingCount > 0) return `↑ ${pendingCount.toLocaleString()}`;
  return formatHeaderStatus(status, pendingCount);
}

function formatHeaderStatus(status: unknown, pendingCount: number) {
  if (pendingCount > 0) return `${pendingCount.toLocaleString()} pending`;
  if (status === "error") return "needs attention";
  if (status === "saving") return "saving";
  if (status === "loading" || status === "opening" || status === "initializing") {
    return "opening";
  }
  return "ready";
}
