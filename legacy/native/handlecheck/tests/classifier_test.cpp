#include "handlecheck.hpp"
#include <nlohmann/json.hpp>

#include <cstdlib>
#include <iostream>
#include <stdexcept>
#include <string>
#include <vector>

namespace {
using nlohmann::json;
int checks = 0;
int failures = 0;

void expect(bool condition, const std::string& label) {
    ++checks;
    if (!condition) {
        ++failures;
        std::cerr << "FAIL: " << label << '\n';
    }
}

const hc::Site& site(const std::string& id) {
    for (const auto& value : hc::sites()) {
        if (value.id == id) return value;
    }
    throw std::runtime_error("Missing catalog site: " + id);
}

hc::HttpResponse response(const hc::Site& adapter, const json& body, long status = 200,
                          const std::string& username = "alice") {
    hc::HttpResponse value;
    value.status = status;
    value.body = body.dump();
    value.headers["content-type"] = "application/json; charset=utf-8";
    value.effective_url = hc::expand_url(adapter.request_template, username);
    value.elapsed_ms = 42;
    return value;
}

void expect_state(const hc::Site& adapter, const hc::HttpResponse& value, hc::State expected,
                  const std::string& label, const std::string& username = "alice") {
    const auto result = hc::classify(adapter, username, value);
    expect(result.state == expected, label + " (got " + hc::state_name(result.state) + ")");
    if (result.state != hc::State::Found) {
        expect(!result.reason.empty(), label + " explains its non-match");
    }
}

void positive_fixtures() {
    const std::vector<std::pair<std::string, json>> fixtures = {
        {"github", {{"login", "alice"}, {"id", 42}, {"type", "User"},
                    {"html_url", "https://github.com/alice"}}},
        {"gitlab", json::array({{{"username", "alice"}, {"id", 42},
                                  {"web_url", "https://gitlab.com/alice"}}})},
        {"codeberg", {{"login", "alice"}, {"id", 42},
                      {"html_url", "https://codeberg.org/alice"}}},
        {"devto", {{"username", "alice"}, {"id", 42}, {"type_of", "user"}}},
        {"lichess", {{"username", "Alice"}, {"id", "alice"}, {"createdAt", 1600000000000LL},
                     {"url", "https://lichess.org/@/Alice"}}},
        {"chess", {{"username", "alice"}, {"player_id", 42},
                   {"url", "https://www.chess.com/member/alice"}}},
        {"huggingface", {{"user", "alice"}, {"_id", "507f1f77bcf86cd799439011"},
                         {"type", "user"}}},
        {"hackernews", {{"id", "alice"}, {"created", 1173923446}, {"karma", 0}}}
    };
    expect(hc::sites().size() == fixtures.size(), "catalog has the eight reviewed adapters");
    for (const auto& fixture : fixtures) {
        const auto& adapter = site(fixture.first);
        const auto result = hc::classify(adapter, "alice", response(adapter, fixture.second));
        expect(result.state == hc::State::Found, fixture.first + " positive profile matches");
        expect(!result.evidence.empty(), fixture.first + " includes positive identity evidence");
        expect(result.site_id == fixture.first, fixture.first + " retains adapter ID");
        expect(result.username == "alice", fixture.first + " retains requested username");
        expect(result.http_status == 200, fixture.first + " retains HTTP status");
        expect(result.elapsed_ms == 42, fixture.first + " retains request timing");

        // A working schema for another account is not evidence about the requested handle.
        auto wrong_user = fixture.second;
        json& object = wrong_user.is_array() ? wrong_user[0] : wrong_user;
        object[json::json_pointer(adapter.identity_pointer)] = "bob";
        expect_state(adapter, response(adapter, wrong_user), hc::State::Unknown,
                     fixture.first + " rejects another account's identity");

        // A status 200 response with a username echoed by a generic endpoint is insufficient.
        json echo = json::object();
        echo[json::json_pointer(adapter.identity_pointer)] = "alice";
        if (adapter.payload_kind == hc::PayloadKind::ExactArray) echo = json::array({echo});
        expect_state(adapter, response(adapter, echo), hc::State::Unknown,
                     fixture.first + " requires independent profile schema");
    }
}

void http_and_format_failures() {
    const auto& github = site("github");
    const json valid = {{"login", "alice"}, {"id", 42}, {"type", "User"}};

    for (const long status : {500L, 502L, 503L}) {
        auto value = response(github, {{"message", "Bad Gateway"}}, status);
        expect_state(github, value, hc::State::Error,
                     "server failure " + std::to_string(status) + " is not a claimed account");
    }
    expect_state(github, response(github, valid, 429), hc::State::RateLimited,
                 "429 is rate limited even if body resembles a profile");
    expect_state(github, response(github, valid, 401), hc::State::Blocked,
                 "401 requires access, not not-found");
    expect_state(github, response(github, valid, 403), hc::State::Blocked,
                 "403 is blocked, not not-found");

    auto quota = response(github, {{"message", "API rate limit exceeded"}}, 403);
    quota.headers["x-ratelimit-remaining"] = "0";
    expect_state(github, quota, hc::State::RateLimited, "GitHub 403 exhausted quota is rate limited");

    for (const long status : {301L, 302L, 307L, 308L}) {
        auto value = response(github, valid, status);
        value.headers["location"] = "https://github.com/login";
        expect_state(github, value, hc::State::Unknown,
                     "redirect " + std::to_string(status) + " never proves a profile");
    }

    auto html = response(github, valid);
    html.headers["content-type"] = "text/html; charset=utf-8";
    html.body = "<!doctype html><title>Just a moment</title><p>Verify you are human</p>";
    const auto html_result = hc::classify(github, "alice", html);
    expect(html_result.state == hc::State::Unknown || html_result.state == hc::State::Blocked,
           "200 HTML challenge is unknown or blocked");

    auto misleading_type = response(github, valid);
    misleading_type.headers["content-type"] = "text/html";
    expect_state(github, misleading_type, hc::State::Unknown,
                 "plausible JSON served as HTML is not silently accepted");
    misleading_type.headers.clear();
    expect_state(github, misleading_type, hc::State::Unknown,
                 "missing media type does not prove the API contract");

    auto mixed_case_headers = response(github, valid);
    mixed_case_headers.headers.clear();
    mixed_case_headers.headers["Content-Type"] = "application/vnd.github+json; charset=utf-8";
    expect_state(github, mixed_case_headers, hc::State::Found,
                 "JSON media type suffix and header case are supported");

    auto followed_redirect = response(github, valid);
    followed_redirect.effective_url = "https://github.com/login";
    expect_state(github, followed_redirect, hc::State::Unknown,
                 "effective URL mismatch cannot silently claim an account");

    auto malformed = response(github, valid);
    malformed.body = "{\"login\":\"alice\",\"id\":42,";
    expect_state(github, malformed, hc::State::Unknown, "truncated JSON remains unknown");
    malformed.body.clear();
    expect_state(github, malformed, hc::State::Unknown, "empty body remains unknown");
    malformed.body = "null";
    expect_state(github, malformed, hc::State::Unknown, "unexpected JSON null remains unknown");
    malformed.body = "[]";
    expect_state(github, malformed, hc::State::Unknown, "unexpected array remains unknown");

    auto network = response(github, valid);
    network.error = "Could not resolve host";
    network.status = 0;
    expect_state(github, network, hc::State::Error, "DNS failure is an error");
    network.error = "Operation timed out";
    expect_state(github, network, hc::State::Error, "timeout is an error");
    network.error = "SSL certificate problem";
    expect_state(github, network, hc::State::Error, "TLS failure is an error");
    network.error = "Response exceeded maximum body size";
    expect_state(github, network, hc::State::Error, "oversize response is an error");
}

void schema_and_negative_semantics() {
    const auto& github = site("github");
    for (const json& bad_id : {json(nullptr), json("42"), json(0), json(-1), json(42.5), json(true)}) {
        expect_state(github,
                     response(github, {{"login", "alice"}, {"id", bad_id}, {"type", "User"}}),
                     hc::State::Unknown, "GitHub ID must be a positive integer: " + bad_id.dump());
    }
    expect_state(github, response(github, {{"login", "alice"}, {"id", 42}, {"type", "error"}}),
                 hc::State::Unknown, "GitHub unrecognized account type is not a profile");
    expect_state(github, response(github, {{"login", 123}, {"id", 42}, {"type", "User"}}),
                 hc::State::Unknown, "identity field must be a string");
    expect_state(github, response(github, {{"login", "ALICE"}, {"id", 42}, {"type", "User"}}),
                 hc::State::Found, "GitHub identity comparison is case insensitive");
    expect_state(github, response(github, {{"message", "Not Found"}}, 404), hc::State::NotFound,
                 "documented GitHub missing profile is not found");
    auto proxy404 = response(github, {{"message", "Not Found"}}, 404);
    proxy404.headers["content-type"] = "text/html";
    proxy404.body = "<h1>404 edge route missing</h1>";
    expect_state(github, proxy404, hc::State::Unknown, "generic edge HTML 404 is not profile absence");
    expect_state(github, response(github, {{"message", "maintenance"}}, 404), hc::State::Unknown,
                 "unrecognized structured 404 remains unknown");

    const auto& codeberg = site("codeberg");
    const std::string swagger = "https://codeberg.org/api/swagger";
    expect_state(codeberg, response(codeberg, {{"message", "user redirect does not exist [name: alice]"},
                                             {"url", swagger}}, 404),
                 hc::State::NotFound, "Codeberg observed missing redirect response names requested user");
    expect_state(codeberg, response(codeberg, {{"message", "user does not exist [name: alice]"},
                                             {"url", swagger}}, 404),
                 hc::State::NotFound, "Codeberg observed missing user response names requested user");
    expect_state(codeberg, response(codeberg, {{"message", "user redirect does not exist [name: bob]"},
                                             {"url", swagger}}, 404),
                 hc::State::Unknown, "Codeberg missing error for a different username proves nothing");
    expect_state(codeberg, response(codeberg, {{"message", "user does not exist [name: alice]"}}, 404),
                 hc::State::Unknown, "Codeberg missing documentation URL remains unknown");
    expect_state(codeberg, response(codeberg, {{"message", "user does not exist [name: alice]"},
                                             {"url", "https://example.test/swagger"}}, 404),
                 hc::State::Unknown, "Codeberg wrong documentation URL remains unknown");
    expect_state(codeberg, response(codeberg, {{"message", "Not Found"}, {"url", swagger}}, 404),
                 hc::State::Unknown, "Codeberg generic route error is not account absence");

    const auto& gitlab = site("gitlab");
    expect_state(gitlab, response(gitlab, json::array()), hc::State::NotFound,
                 "GitLab exact username filter empty array has no visible profile");
    expect_state(gitlab, response(gitlab, {{"username", "alice"}, {"id", 42}}), hc::State::Unknown,
                 "GitLab requires documented array shape");

    const auto& hn = site("hackernews");
    expect_state(hn, response(hn, {{"id", "Alice"}, {"created", 1173923446}, {"karma", 0}}),
                 hc::State::Unknown, "HN usernames are case sensitive");
    expect_state(hn, response(hn, {{"id", "alice"}, {"created", 1173923446}, {"karma", -1}}),
                 hc::State::Found, "HN negative karma is valid profile data");
    const auto hn_null = hc::classify(hn, "alice", response(hn, nullptr));
    expect(hn_null.state == hc::State::NotFound, "HN null means no public API record");
    expect(hn_null.reason.find("public") != std::string::npos,
           "HN null explanation preserves public-visibility limitation");

    const auto& hf = site("huggingface");
    expect_state(hf, response(hf, {{"user", "alice"}, {"_id", "not-an-id"}, {"type", "user"}}),
                 hc::State::Unknown, "HF rejects invalid stable ID");
    expect_state(hf, response(hf, {{"user", "alice"}, {"_id", "507f1f77bcf86cd799439011"},
                                 {"type", "org"}}),
                 hc::State::Unknown, "HF user endpoint rejects organization schema");

    const auto& lichess = site("lichess");
    expect_state(lichess, response(lichess, {{"username", "alice"}, {"id", "bob"},
                                         {"createdAt", 1600000000000LL}}),
                 hc::State::Unknown, "Lichess ID must agree with requested handle");
    expect_state(lichess, response(lichess, {{"username", "alice"}, {"id", "alice"}}),
                 hc::State::Unknown, "Lichess needs non-echo creation timestamp evidence");
    for (const json& bad_created : {json(0), json("1600000000000"), json(nullptr)}) {
        expect_state(lichess, response(lichess, {{"username", "alice"}, {"id", "alice"},
                                             {"createdAt", bad_created}}),
                     hc::State::Unknown, "Lichess creation timestamp must be a positive integer");
    }
}

void input_and_encoding() {
    expect(hc::percent_encode("a/b?c=d&x y") == "a%2Fb%3Fc%3Dd%26x%20y",
           "URL encoding prevents path/query interpolation");
    expect(hc::percent_encode("AZaz09-._~") == "AZaz09-._~", "RFC3986 unreserved bytes stay intact");
    expect(hc::expand_url("https://example.test/u/{username}", "a/b") ==
               "https://example.test/u/a%2Fb", "template expansion encodes username");
    expect(hc::ascii_lower("Alice-123") == "alice-123", "ASCII case folding is deterministic");
    for (const auto& adapter : hc::sites()) {
        expect(hc::valid_username(adapter, "alice"), adapter.id + " accepts ordinary handle");
        for (const auto& bad : {"", "../alice", "alice/bob", "alice?admin=1", "alice bob", "alice\n"}) {
            expect(!hc::valid_username(adapter, bad), adapter.id + " rejects unsafe username " + json(bad).dump());
        }
    }
}
} // namespace

int main() {
    try {
        positive_fixtures();
        http_and_format_failures();
        schema_and_negative_semantics();
        input_and_encoding();
    } catch (const std::exception& error) {
        std::cerr << "FAIL: test exception: " << error.what() << '\n';
        return EXIT_FAILURE;
    }
    std::cout << checks << " classifier checks, " << failures << " failures\n";
    return failures == 0 ? EXIT_SUCCESS : EXIT_FAILURE;
}
