#include "handlecheck.hpp"

namespace hc {

const std::vector<Site>& sites() {
    // These adapters inspect public profile APIs, not signup/recovery endpoints.
    // A negative result concerns the API's visible profile record, not availability
    // of the handle or whether profiles on different sites share an owner.
    static const std::vector<Site> catalog = {
        {"github", "GitHub", "https://api.github.com/users/{username}",
         "https://github.com/{username}", "[A-Za-z0-9][A-Za-z0-9-]{0,38}",
         "/login", {"/id", "/type"}, PayloadKind::Object, false, true,
         "octocat", "https://docs.github.com/en/rest/users/users#get-a-user"},
        {"gitlab", "GitLab", "https://gitlab.com/api/v4/users?username={username}",
         "https://gitlab.com/{username}", "[A-Za-z0-9_][A-Za-z0-9_.-]{0,254}",
         "/username", {"/id"}, PayloadKind::ExactArray, false, false,
         "gitlab-bot", "https://docs.gitlab.com/api/users/#list-all-users"},
        // Forgejo installations can restrict anonymous visibility. Only the
        // deployed API's exact missing-user schema is accepted (live checked
        // 2026-11-11); a generic 404 remains unknown.
        {"codeberg", "Codeberg", "https://codeberg.org/api/v1/users/{username}",
         "https://codeberg.org/{username}", "[A-Za-z0-9_][A-Za-z0-9_.-]{0,99}",
         "/login", {"/id"}, PayloadKind::Object, false, true,
         "forgejo", "https://codeberg.org/api/swagger"},
        {"devto", "DEV", "https://dev.to/api/users/by_username?url={username}",
         "https://dev.to/{username}", "[A-Za-z0-9_]{1,30}",
         "/username", {"/id", "/type_of"}, PayloadKind::Object, false, true,
         "ben", "https://developers.forem.com/api/v0#tag/users/operation/getUser"},
        // Lichess requires serial requests to its API. The caller schedules one
        // request per site and stops rather than retrying a rate-limited request.
        {"lichess", "Lichess", "https://lichess.org/api/user/{username}",
         "https://lichess.org/@/{username}", "[A-Za-z0-9_-]{2,30}",
         "/username", {"/id", "/createdAt"}, PayloadKind::Object, false, true,
         "thibault", "https://lichess.org/api#tag/account/GET/api/user/{username}"},
        {"chess", "Chess.com", "https://api.chess.com/pub/player/{username}",
         "https://www.chess.com/member/{username}", "[A-Za-z0-9_-]{3,25}",
         "/username", {"/player_id"}, PayloadKind::Object, false, true,
         "hikaru", "https://support.chess.com/en/articles/9650547-what-is-the-pubapi-and-how-do-i-use-it"},
        // This route covers users, not organization namespaces.
        {"huggingface", "Hugging Face", "https://huggingface.co/api/users/{username}/overview",
         "https://huggingface.co/{username}", "[A-Za-z0-9][A-Za-z0-9_-]{0,41}",
         "/user", {"/_id", "/type"}, PayloadKind::Object, false, true,
         "Wauplin", "https://huggingface.co/docs/huggingface_hub/en/package_reference/hf_api#huggingface_hub.HfApi.get_user_overview"},
        // HN IDs are case-sensitive. Its API excludes users with no public
        // activity, so null does not prove the account itself does not exist.
        {"hackernews", "Hacker News", "https://hacker-news.firebaseio.com/v0/user/{username}.json",
         "https://news.ycombinator.com/user?id={username}", "[A-Za-z0-9_-]{1,20}",
         "/id", {"/created", "/karma"}, PayloadKind::NullableObject, true, false,
         "jl", "https://github.com/HackerNews/API#users"}
    };
    return catalog;
}

} // namespace hc
