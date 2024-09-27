javascript: (function () {
  var currentUrl = encodeURIComponent(window.location.href);
  var tags = prompt("Enter tags (comma-separated):", "");
  var dbPath = "/path/to/research.sqlite";
  var provider = "pocket";
  if (tags !== null && dbPath !== null) {
    var encodedTags = encodeURIComponent(tags);
    var encodedDbPath = encodeURIComponent(dbPath);
    var researchUrl = `research://save?url=${currentUrl}&provider=${provider}&tags=${encodedTags}&db_path=${encodedDbPath}`;
    window.location.href = researchUrl;
  }
})();
