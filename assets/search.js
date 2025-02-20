/// <reference types="./types.d.ts" />
import Fuse from "https://cdn.jsdelivr.net/npm/fuse.js@7.0.0/dist/fuse.mjs";

const options = {
  includeScore: true,
  keys: ["tags", "excerpt", "title"],
  shouldSort: true,
  threshold: 0.4,
  useExtendedSearch: true,
};

const fuse = new Fuse(item_tags, options);
const searchForm = document.getElementById("searchForm");
const searchInput = document.getElementById("searchInput");
const resultsContainer = document.getElementById("resultsContainer");
const resultTemplate = document.getElementById("resultTemplate");
const tagInput = document.getElementById("tagInput");
const tagsContainer = document.getElementById("tagsContainer");

let activeTags = [];

/**
 * @param {string} searchQuery
 * @returns {item_tag[]}
 */
function searchItems(searchQuery) {
  const results = fuse.search(searchQuery);
  return results.map((result) => result.item);
}

/**
 * @param {item_tag[]} items
 */
function renderResults(items) {
  const fragment = document.createDocumentFragment();

  items.forEach((item) => {
    const clone = resultTemplate.content.cloneNode(true);
    clone.querySelector("a").href = item.uri;
    clone.querySelector("h3").textContent = item.title;
    clone.querySelector("p").textContent = item.excerpt || "No excerpt available";
    clone.querySelector(".time-added").textContent = new Date(item.time_added).toLocaleDateString();

    const domain = item.uri.split('/')[2];
    clone.querySelector(".domain").textContent = domain || item.uri.slice(0, 10);
    
    const tagsContainer = clone.querySelector(".tags-container");
    item.tags.forEach((tag) => {
      const span = document.createElement("span");
      span.className = "text-xs px-2 py-0.5 text-gray-500";
      span.textContent = tag;
      tagsContainer.appendChild(span);
    });

    fragment.appendChild(clone);
  });

  resultsContainer.innerHTML = "";
  resultsContainer.appendChild(fragment);
}

/**
 * Debounce helper
 * @param {Function} func
 * @param {number} wait
 */
function debounce(func, wait) {
  let timeout;
  return function (...args) {
    clearTimeout(timeout);
    timeout = setTimeout(() => func.apply(this, args), wait);
  };
}

/**
 * Filter results based on user input
 * @param {item_tag[]} items 
 * @param {Object} filters
 * @returns {item_tag[]}
 */
function filterResults(items, filters) {
  let filteredItems = items;

  if (filters.tags.length > 0) {
    filteredItems = filteredItems.filter(item => filters.tags.every(tag => item.tags.includes(tag)));
  }

  if (filters.dateFrom) {
    const dateFrom = new Date(filters.dateFrom);
    filteredItems = filteredItems.filter(item => new Date(item.time_added) >= dateFrom);
  }

  if (filters.dateTo) {
    const dateTo = new Date(filters.dateTo);
    filteredItems = filteredItems.filter(item => new Date(item.time_added) <= dateTo);
  }

  if (filters.favorite) {
    filteredItems = filteredItems.filter(item => item.favorite);
  }

  return filteredItems;
}

/**
 * Handle tag input keypress
 * @param {KeyboardEvent} e 
 */
function handleTagInputKeypress(e) {
  if (e.key === "Enter") {
    e.preventDefault();
    const tagValue = tagInput.value.trim();
    if (tagValue && !activeTags.includes(tagValue)) {
      activeTags.push(tagValue);
      const tagElem = document.createElement("span");
      tagElem.className = "tag";
      tagElem.textContent = tagValue;
      tagElem.addEventListener("click", () => {
        activeTags = activeTags.filter(tag => tag !== tagValue);
        tagElem.remove();
        handleFilterChange();
      });
      tagsContainer.appendChild(tagElem);
      tagInput.value = "";
      handleFilterChange();
    }
  }
}

/**
 * Handle search input and filters
 */
function handleFilterChange() {
  const searchQuery = searchInput.value.trim();
  let matchedItems = searchItems(searchQuery);

  const filters = {
    tags: activeTags,
    dateFrom: document.getElementById("dateFrom").value,
    dateTo: document.getElementById("dateTo").value,
    favorite: document.getElementById("favoriteFilter").checked,
  };

  matchedItems = filterResults(matchedItems, filters);
  renderResults(matchedItems);
}

// Attach event listeners
searchInput.addEventListener("input", debounce(handleFilterChange, 300));
tagInput.addEventListener("keypress", handleTagInputKeypress);
searchForm.addEventListener("input", debounce(handleFilterChange, 300));
searchForm.addEventListener("submit", (e) => {
  e.preventDefault();
  handleFilterChange();
});
