/** @typedef {import("fuse.js").Fuse} Fuse */


/**
 * @typedef item_tag
 * @property {string[]} tags
 * @property {number} id
 * @property {string} uri
 * @property {string} title
 * @property {string} excerpt
 * @property {number} time_added
 * @property {boolean} favorite
 * @property {string} lang
 */

const options = {
  includeScore: true,
  keys: ["tags", "excerpt", "title"],
  logicalOperator: "or",
  useExtendedSearch: true,
};

/** @type {item_tag[]} */
var item_tags;

const fuse = new Fuse(item_tags, options);

/**
 * @param {string} searchQuery
 * @returns {item_tag[]}
 */
function searchItems(searchQuery) {
  const tags = searchQuery.split(",").map((tag) => tag.trim());
  const pattern = tags.map((tag) => `'${tag}`).join("|");
  const results = fuse.search(pattern);

  return results.map((result) => result.item);
}

const searchBtn = document.getElementById("searchBtn");
const searchInput = document.getElementById("searchInput");
const resultsContainer = document.getElementById("resultsContainer");

searchBtn.addEventListener("click", () => {
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
<h3 class="text-lg font-bold">${item.title}</h3>
<ul class="inline-flex flex-wrap gap-2" >
${tagsHtml}
</ul>
<p class="break-words">${item.excerpt || "No excerpt available"}</p>
<a href="${item.uri}" target="_blank" class="text-blue-500 hover:underline">Read more</a>
`;
    resultsContainer.appendChild(itemElement);
  });
});
