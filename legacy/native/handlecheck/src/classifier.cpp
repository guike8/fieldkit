#include "handlecheck.hpp"
#include <nlohmann/json.hpp>

#include <algorithm>
#include <cctype>
#include <regex>
#include <stdexcept>

namespace hc {
namespace {

using Json = nlohmann::json;

std::string trim(std::string value) {
    const auto first = value.find_first_not_of(" \t\r\n");
    if (first == std::string::npos) return {};
    return value.substr(first, value.find_last_not_of(" \t\r\n") - first + 1);
}

std::string header(const HttpResponse& response, const std::string& name) {
    for (const auto& item : response.headers) {
        if (ascii_lower(item.first) == name) return trim(item.second);
    }
    return {};
}

bool json_content_type(const HttpResponse& response) {
    auto type = ascii_lower(header(response, "content-type"));
    type = trim(type.substr(0, type.find(';')));
    return type == "application/json" ||
           (type.size() > 5 && type.find('/') != std::string::npos &&
            type.compare(type.size() - 5, 5, "+json") == 0);
}

bool positive_integer(const Json& value) {
    return value.is_number_integer() &&
           (value.is_number_unsigned() ? value.get<unsigned long long>() > 0 :
                                        value.get<long long>() > 0);
}

bool same_identity(const Site& site, const std::string& actual, const std::string& expected) {
    return site.case_sensitive ? actual == expected : ascii_lower(actual) == ascii_lower(expected);
}

bool required_value_valid(const Site& site, const std::string& pointer,
                          const Json& value, const std::string& username) {
    if (pointer == "/id") {
        if (site.id == "lichess") {
            return value.is_string() && value.get<std::string>() == ascii_lower(username);
        }
        return positive_integer(value);
    }
    if (pointer == "/player_id" || pointer == "/created" || pointer == "/createdAt") return positive_integer(value);
    if (pointer == "/karma") return value.is_number_integer();
    if (pointer == "/_id") {
        if (!value.is_string()) return false;
        const auto stable_id = value.get<std::string>();
        return std::regex_match(stable_id, std::regex("[A-Fa-f0-9]{24}"));
    }
    if (pointer == "/type_of") return value.is_string() && value == "user";
    if (pointer == "/type") {
        if (!value.is_string()) return false;
        if (site.id == "github") return value == "User" || value == "Organization" || value == "Bot";
        if (site.id == "huggingface") return value == "user";
        return !value.get<std::string>().empty();
    }
    // Unknown schema requirements fail closed unless they have a concrete,
    // non-null scalar value; production adapters use the explicit cases above.
    return (value.is_string() && !value.get<std::string>().empty()) || positive_integer(value);
}

bool object_matches(const Site& site, const Json& object, const std::string& username,
                    std::vector<Evidence>& evidence, std::string& reason) {
    if (!object.is_object()) {
        reason = "API response does not contain the expected profile object";
        return false;
    }
    try {
        const auto identity = Json::json_pointer(site.identity_pointer);
        if (!object.contains(identity) || !object.at(identity).is_string()) {
            reason = "Expected identity field is missing or has changed type";
            return false;
        }
        const auto actual = object.at(identity).get<std::string>();
        if (!same_identity(site, actual, username)) {
            reason = "Returned identity does not match the requested username";
            return false;
        }
        for (const auto& field : site.required_pointers) {
            const auto pointer = Json::json_pointer(field);
            if (!object.contains(pointer) || !required_value_valid(site, field, object.at(pointer), username)) {
                reason = "Expected profile schema is missing or invalid at " + field;
                return false;
            }
        }
        evidence.push_back({site.identity_pointer, username, actual});
        reason = "Public API returned a matching identity and validated profile ID/schema";
        return true;
    } catch (const Json::exception&) {
        reason = "Expected profile schema could not be read";
        return false;
    }
}

std::string string_field(const Json& object, const char* field) {
    const auto it = object.find(field);
    return it != object.end() && it->is_string() ? ascii_lower(trim(it->get<std::string>())) : "";
}

bool missing_profile_error(const Site& site, const Json& body, const std::string& username) {
    if (!site.json_404_is_not_found || !body.is_object()) return false;
    const auto message = string_field(body, "message");
    const auto error = string_field(body, "error");
    if (site.id == "github") return message == "not found";
    if (site.id == "codeberg") {
        const auto handle = ascii_lower(username);
        return string_field(body, "url") == "https://codeberg.org/api/swagger" &&
            (message == "user redirect does not exist [name: " + handle + "]" ||
             message == "user does not exist [name: " + handle + "]");
    }
    if (site.id == "devto") {
        return (error == "not found" || error == "user not found") &&
               body.contains("status") && body.at("status").is_number_integer() && body.at("status") == 404;
    }
    if (site.id == "lichess") return error == "not found" || error == "not found." || error == "user not found";
    if (site.id == "chess") {
        if (!body.contains("code") || !body.at("code").is_number_integer()) return false;
        const auto quoted = "user \"" + ascii_lower(username) + "\" not found";
        return message == quoted || message == quoted + "." || message == "user not found" || message == "user not found.";
    }
    if (site.id == "huggingface") {
        return error == "user not found" || error == "user not found." ||
               error == "this user does not exist" || error == "this user does not exist.";
    }
    return false;
}

bool looks_like_challenge(const HttpResponse& response) {
    const auto prefix = ascii_lower(response.body.substr(0, 16384));
    // Inspect HTML only: public profile strings can themselves contain these
    // words and should not be mistaken for a transport challenge.
    if (prefix.find('<') == std::string::npos) return false;
    return prefix.find("<title>just a moment") != std::string::npos ||
           prefix.find("cf-chl-") != std::string::npos ||
           prefix.find("<title>access denied") != std::string::npos ||
           prefix.find("<title>verify you are human") != std::string::npos ||
           prefix.find("<title>making sure you're not a bot") != std::string::npos;
}

} // namespace

std::string state_name(State state) {
    switch (state) {
        case State::Found: return "found";
        case State::NotFound: return "not_found";
        case State::Blocked: return "blocked";
        case State::RateLimited: return "rate_limited";
        case State::Unknown: return "unknown";
        case State::Error: return "error";
        case State::Invalid: return "invalid";
    }
    return "unknown";
}

std::string ascii_lower(std::string value) {
    for (char& ch : value) {
        if (ch >= 'A' && ch <= 'Z') ch = static_cast<char>(ch - 'A' + 'a');
    }
    return value;
}

std::string percent_encode(const std::string& value) {
    static const char digits[] = "0123456789ABCDEF";
    std::string encoded;
    for (const unsigned char ch : value) {
        if ((ch >= 'A' && ch <= 'Z') || (ch >= 'a' && ch <= 'z') ||
            (ch >= '0' && ch <= '9') || ch == '-' || ch == '_' || ch == '.' || ch == '~') {
            encoded += static_cast<char>(ch);
        } else {
            encoded += '%';
            encoded += digits[ch >> 4];
            encoded += digits[ch & 15];
        }
    }
    return encoded;
}

std::string expand_url(const std::string& pattern, const std::string& username) {
    auto expanded = pattern;
    const auto encoded = percent_encode(username);
    for (const auto* marker : {"{username}", "{}"}) {
        std::size_t position = 0;
        const std::string placeholder(marker);
        while ((position = expanded.find(placeholder, position)) != std::string::npos) {
            expanded.replace(position, placeholder.size(), encoded);
            position += encoded.size();
        }
    }
    return expanded;
}

bool valid_username(const Site& site, const std::string& username) {
    if (username.empty() || username.size() > 255) return false;
    for (const unsigned char ch : username) {
        if (ch < 33 || ch > 126 || ch == '/' || ch == '\\' || ch == '?' || ch == '#' || ch == '%') return false;
    }
    try {
        return std::regex_match(username, std::regex(site.username_pattern));
    } catch (const std::regex_error&) {
        return false;
    }
}

Result classify(const Site& site, const std::string& username, const HttpResponse& response) {
    Result result;
    result.site_id = site.id;
    result.site_name = site.name;
    result.username = username;
    result.profile_url = expand_url(site.profile_template, username);
    result.request_url = expand_url(site.request_template, username);
    result.http_status = response.status;
    result.elapsed_ms = response.elapsed_ms;
    if (!valid_username(site, username)) {
        result.state = State::Invalid;
        result.reason = "Username does not match this site's accepted lookup format";
        return result;
    }
    if (!response.error.empty() || response.status == 0) {
        result.state = State::Error;
        result.reason = response.error.empty() ? "No HTTP response was received" : response.error;
        return result;
    }
    if (response.status == 429 ||
        (site.id == "github" && response.status == 403 && header(response, "x-ratelimit-remaining") == "0")) {
        result.state = State::RateLimited;
        result.reason = "API rate limit reached; no conclusion about this username";
        const auto retry_after = header(response, "retry-after");
        if (!retry_after.empty() && retry_after.size() <= 12 &&
            std::all_of(retry_after.begin(), retry_after.end(), [](char ch) { return ch >= '0' && ch <= '9'; })) {
            result.reason += " (Retry-After: " + retry_after + " seconds)";
        }
        return result;
    }
    if (response.status == 401 || response.status == 403) {
        result.state = State::Blocked;
        result.reason = response.status == 401 ? "API requires authentication" : "API denied this request";
        return result;
    }
    if (response.status >= 500 && response.status <= 599) {
        result.state = State::Error;
        result.reason = "Upstream server error; no conclusion about this username";
        return result;
    }
    if ((response.status >= 300 && response.status <= 399) ||
        (!response.effective_url.empty() && response.effective_url != result.request_url)) {
        result.reason = "Redirect or changed request URL cannot verify this username";
        return result;
    }
    if (!json_content_type(response)) {
        if (looks_like_challenge(response)) {
            result.state = State::Blocked;
            result.reason = "API returned an HTML access challenge";
        } else {
            result.reason = "API did not return a JSON content type";
        }
        return result;
    }
    const auto body = Json::parse(response.body, nullptr, false);
    if (body.is_discarded()) {
        result.reason = "API returned malformed JSON";
        return result;
    }
    if (response.status == 404) {
        if (missing_profile_error(site, body, username)) {
            result.state = State::NotFound;
            result.reason = "Public profile API returned its recognized missing-user error";
        } else {
            result.reason = "404 response does not prove absence of a public profile";
        }
        return result;
    }
    if (response.status != 200) {
        result.reason = "Unexpected HTTP status from public profile API";
        return result;
    }
    if (site.payload_kind == PayloadKind::NullableObject && body.is_null()) {
        result.state = State::NotFound;
        result.reason = "No public API record; Hacker News omits users without public activity";
        return result;
    }
    if (site.payload_kind == PayloadKind::ExactArray) {
        if (!body.is_array()) {
            result.reason = "API response does not contain the expected exact-match array";
            return result;
        }
        if (body.empty()) {
            result.state = State::NotFound;
            result.reason = "Exact username API returned no visible public profile";
            return result;
        }
        if (body.size() != 1) {
            result.reason = "Exact username API returned an ambiguous number of profiles";
            return result;
        }
        if (object_matches(site, body.front(), username, result.evidence, result.reason)) result.state = State::Found;
        return result;
    }
    if (object_matches(site, body, username, result.evidence, result.reason)) result.state = State::Found;
    return result;
}

} // namespace hc
