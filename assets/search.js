/// <reference types="./types.d.ts" />
import Fuse from "https://cdn.jsdelivr.net/npm/fuse.js@7.0.0/dist/fuse.mjs";

const options = {
  includeScore: true,
  keys: ["tags", "excerpt", "title"],
  logicalOperator: "or",
  useExtendedSearch: true,
};

const fuse = new Fuse(item_tags, options);

/**
 * @param {string} searchQuery
 * @returns {item_tag[]}
 */
function searchItems(searchQuery) {
  const tags = searchQuery.split(",").map((tag) => tag.trim());
  const pattern = tags.map(/** @param {string} tag */ (tag) => `'${tag}`).join(
    "|",
  );
  const results = fuse.search(pattern);

  return results.map(/** @param {{item: item_tag}} result */ (result) =>
    result.item
  );
}

const searchInput = document.getElementById("searchInput");
const resultsContainer = document.getElementById("resultsContainer");

/**
 * @param {Event} e
 */
function handleSearch(e) {
  e.preventDefault();
  const searchQuery = searchInput.value;
  const matchedItems = searchItems(searchQuery);

  resultsContainer.innerHTML = "";

  matchedItems.forEach((item) => {
    const itemElement = document.createElement("li");
    itemElement.style = "background-color:#eee";
    itemElement.className = "p-2 rounded flex flex-col gap-2 shadow";

    const tagsHtml = item.tags.map((tag) =>
      `<li class="pointer p-2 rounded" style="background-color:#ccf">${tag}</li>`
    ).join("");

    itemElement.innerHTML = `
<h3 class="text-lg font-bold break-words">${item.title}</h3>
<ul class="inline-flex flex-wrap gap-2" >
${tagsHtml}
</ul>
<a href="${item.uri}" target="_blank">
<p class="break-words">${item.excerpt || "No excerpt available"}</p>
<span class="text-blue-500 hover:underline">Read more</span>
</a>
`;
    resultsContainer.appendChild(itemElement);
  });
  return false;
}

document.getElementById("searchForm").addEventListener(
  "submit",
  handleSearch,
);
