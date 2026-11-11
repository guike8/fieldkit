#include "handlecheck.hpp"
#include <nlohmann/json.hpp>
#include <algorithm>
#include <atomic>
#include <cerrno>
#include <chrono>
#include <cmath>
#include <cstdlib>
#include <cstring>
#include <ctime>
#include <fcntl.h>
#include <iomanip>
#include <iostream>
#include <mutex>
#include <optional>
#include <random>
#include <regex>
#include <sstream>
#include <stdexcept>
#include <thread>
#include <unistd.h>

using json = nlohmann::json;
namespace {
struct Options {
    std::string username;
    std::vector<std::string> site_ids;
    std::string output;
    int jobs = 4;
    long timeout_ms = 10000;
    bool json_stdout = false;
    bool doctor = false;
    bool list = false;
};
struct Outcome {
    hc::Result result;
    std::optional<hc::Result> negative_control;
};

std::string timestamp() {
    const auto now = std::chrono::system_clock::now();
    const std::time_t time = std::chrono::system_clock::to_time_t(now);
    std::tm utc{};
    gmtime_r(&time, &utc);
    std::ostringstream out;
    out << std::put_time(&utc, "%Y-%m-%dT%H:%M:%SZ");
    return out.str();
}

void usage() {
    std::cout << "Handlecheck 0.1.0 - evidence-based public username lookup\n\n"
        "Usage: handlecheck USERNAME [options]\n"
        "       handlecheck --doctor [--site SITE] [options]\n"
        "       handlecheck --list-sites\n\n"
        "  --site ID[,ID...]  Check selected sites; repeatable\n"
        "  --json            Print the complete JSON report to stdout\n"
        "  --output FILE     Also save JSON to a new file (never overwrites)\n"
        "  --timeout SECONDS Per-request timeout, 1-60 (default: 10)\n"
        "  --jobs N          Concurrent sites, 1-8 (default: 4)\n"
        "  --doctor          Test known public profiles and random negative controls\n"
        "  --list-sites      Show supported sites, examples, and documentation\n"
        "  --help            Show this help\n"
        "  --version         Print version\n\n"
        "Matches require explicit profile identity evidence. Each apparent match\n"
        "gets a random negative-control check. Blocked/failed checks stay unresolved.\n"
        "A shared handle does not establish that accounts belong to the same person.\n"
        "Exit codes: 0 complete, 1 usage/output error, 2 unresolved checks.\n";
}

double number(const std::string& text, const std::string& option) {
    char* end = nullptr;
    errno = 0;
    const double value = std::strtod(text.c_str(), &end);
    if (errno || end == text.c_str() || *end || !std::isfinite(value))
        throw std::runtime_error("Invalid number for " + option);
    return value;
}

Options parse(int argc, char** argv) {
    Options options;
    for (int i = 1; i < argc; ++i) {
        const std::string argument = argv[i];
        const auto value = [&]() -> std::string {
            if (i + 1 >= argc) throw std::runtime_error("Missing value for " + argument);
            return argv[++i];
        };
        if (argument == "--help" || argument == "-h") { usage(); std::exit(0); }
        else if (argument == "--version") { std::cout << "handlecheck 0.1.0\n"; std::exit(0); }
        else if (argument == "--site") {
            std::istringstream ids(value());
            std::string id;
            while (std::getline(ids, id, ',')) {
                if (id.empty()) throw std::runtime_error("Empty site ID");
                options.site_ids.push_back(hc::ascii_lower(id));
            }
            if (options.site_ids.empty()) throw std::runtime_error("Empty site list");
        } else if (argument == "--output") options.output = value();
        else if (argument == "--json") options.json_stdout = true;
        else if (argument == "--doctor") options.doctor = true;
        else if (argument == "--list-sites") options.list = true;
        else if (argument == "--timeout") {
            const double seconds = number(value(), argument);
            if (seconds < 1 || seconds > 60) throw std::runtime_error("Timeout must be 1-60 seconds");
            options.timeout_ms = static_cast<long>(seconds * 1000);
        } else if (argument == "--jobs") {
            const double jobs = number(value(), argument);
            if (jobs < 1 || jobs > 8 || std::floor(jobs) != jobs)
                throw std::runtime_error("Jobs must be an integer from 1 to 8");
            options.jobs = static_cast<int>(jobs);
        } else if (!argument.empty() && argument[0] == '-')
            throw std::runtime_error("Unknown option: " + argument);
        else if (options.username.empty()) options.username = argument;
        else throw std::runtime_error("Supply one username per run");
    }
    if (options.list && (options.doctor || !options.username.empty()))
        throw std::runtime_error("--list-sites cannot be combined with a search or --doctor");
    if (options.doctor && !options.username.empty())
        throw std::runtime_error("--doctor uses built-in public fixtures; omit USERNAME");
    if (!options.list && !options.doctor) {
        if (!options.username.empty() && options.username[0] == '@') options.username.erase(0, 1);
        if (!std::regex_match(options.username, std::regex("[A-Za-z0-9_.-]{1,64}")))
            throw std::runtime_error("Supply a username of 1-64 ASCII letters, digits, dots, underscores or hyphens");
    }
    return options;
}

std::string random_handle() {
    std::random_device random;
    std::ostringstream out;
    out << "hc";
    // 48 random bits, under the shortest supported maximum username length.
    for (int i = 0; i < 6; ++i)
        out << std::hex << std::setw(2) << std::setfill('0') << (random() & 0xff);
    return out.str();
}

hc::Result check(const hc::Site& site, const std::string& username, const Options& options) {
    hc::HttpResponse response;
    if (hc::valid_username(site, username))
        response = hc::fetch(hc::expand_url(site.request_template, username), {options.timeout_ms});
    auto result = hc::classify(site, username, response);
    result.checked_at = timestamp();
    return result;
}

Outcome scan(const hc::Site& site, const Options& options) {
    Outcome outcome;
    outcome.result = check(site, options.doctor ? site.example_username : options.username, options);
    auto& result = outcome.result;
    if (result.state == hc::State::Found) {
        std::string control;
        do { control = random_handle(); } while (hc::ascii_lower(control) == hc::ascii_lower(result.username));
        outcome.negative_control = check(site, control, options);
        if (outcome.negative_control->state == hc::State::NotFound) {
            result.control = "passed";
            if (options.doctor) result.reason = "Healthy: known profile verified and random control returned no public record";
        } else {
            result.state = hc::State::Unknown;
            result.control = "failed";
            result.reason = "Match withheld: random negative control was " +
                hc::state_name(outcome.negative_control->state) + "; " + outcome.negative_control->reason;
        }
    } else {
        result.control = "not_run";
        if (options.doctor) {
            const auto original = hc::state_name(result.state);
            // A missing known-positive fixture is a failed health check, not an absent searched user.
            if (result.state == hc::State::NotFound || result.state == hc::State::Invalid)
                result.state = hc::State::Unknown;
            result.reason = "Health check failed (known profile " + original + "): " + result.reason;
        }
    }
    return outcome;
}

json result_json(const hc::Result& result) {
    json evidence = json::array();
    for (const auto& entry : result.evidence)
        evidence.push_back({{"field", entry.field}, {"expected", entry.expected}, {"actual", entry.actual}});
    return {{"site", result.site_id}, {"site_name", result.site_name}, {"username", result.username},
        {"status", hc::state_name(result.state)}, {"reason", result.reason},
        {"profile_url", result.profile_url}, {"request_url", result.request_url},
        {"http_status", result.http_status}, {"elapsed_ms", result.elapsed_ms},
        {"checked_at", result.checked_at}, {"control", result.control}, {"evidence", evidence}};
}

std::string clean(std::string value) {
    for (auto& ch : value) if (static_cast<unsigned char>(ch) < 32 || ch == 127) ch = ' ';
    return value;
}

void write_new_file(const std::string& path, const std::string& contents) {
    const int file = ::open(path.c_str(), O_WRONLY | O_CREAT | O_EXCL, 0600);
    if (file < 0) throw std::runtime_error("Cannot create report '" + path + "': " + std::strerror(errno));
    size_t offset = 0;
    while (offset < contents.size()) {
        const auto wrote = ::write(file, contents.data() + offset, contents.size() - offset);
        if (wrote < 0 && errno == EINTR) continue;
        if (wrote <= 0) {
            const std::string error = std::strerror(errno);
            ::close(file);
            ::unlink(path.c_str());
            throw std::runtime_error("Could not write report: " + error);
        }
        offset += static_cast<size_t>(wrote);
    }
    if (::close(file) != 0) throw std::runtime_error("Could not close report file");
}
} // namespace

int main(int argc, char** argv) {
    bool http_ready = false;
    try {
        if (argc == 1) { usage(); return 1; }
        const Options options = parse(argc, argv);
        std::vector<hc::Site> selected;
        for (const auto& id : options.site_ids) {
            const auto found = std::find_if(hc::sites().begin(), hc::sites().end(),
                [&](const hc::Site& site) { return site.id == id; });
            if (found == hc::sites().end()) throw std::runtime_error("Unknown site '" + id + "'; use --list-sites");
        }
        for (const auto& site : hc::sites())
            if (options.site_ids.empty() || std::find(options.site_ids.begin(), options.site_ids.end(), site.id) != options.site_ids.end())
                selected.push_back(site);
        if (options.list) {
            for (const auto& site : selected)
                std::cout << site.id << "\t" << site.name << "\n  Example: " << site.example_username
                    << "\n  Docs: " << site.docs_url << "\n";
            return 0;
        }
        if (!options.doctor && std::none_of(selected.begin(), selected.end(),
            [&](const hc::Site& site) { return hc::valid_username(site, options.username); }))
            throw std::runtime_error("No selected site accepts this username format; no requests sent");
        http_ready = hc::http_global_init();
        if (!http_ready) throw std::runtime_error("Could not initialize libcurl");
        const std::string started_at = timestamp();
        std::cerr << (options.doctor ? "Testing " : "Checking ") << selected.size()
            << " public-profile APIs; up to " << std::min(options.jobs, static_cast<int>(selected.size()))
            << " concurrent sites.\n";
        std::vector<Outcome> outcomes(selected.size());
        std::atomic<size_t> next{0};
        std::vector<std::thread> workers;
        struct JoinWorkers {
            std::vector<std::thread>& workers;
            ~JoinWorkers() { for (auto& worker : workers) if (worker.joinable()) worker.join(); }
        } join_workers{workers};
        for (int i = 0; i < std::min(options.jobs, static_cast<int>(selected.size())); ++i)
            workers.emplace_back([&]() {
                while (true) {
                    const size_t index = next.fetch_add(1);
                    if (index >= selected.size()) break;
                    try { outcomes[index] = scan(selected[index], options); }
                    catch (const std::exception& error) {
                        auto& result = outcomes[index].result;
                        result.site_id = selected[index].id;
                        result.site_name = selected[index].name;
                        result.username = options.doctor ? selected[index].example_username : options.username;
                        result.state = hc::State::Error;
                        result.reason = error.what();
                        result.checked_at = timestamp();
                    }
                }
            });
        for (auto& worker : workers) worker.join();
        hc::http_global_cleanup();
        http_ready = false;
        json results = json::array();
        json summary = {{"found", 0}, {"not_found", 0}, {"blocked", 0}, {"rate_limited", 0},
            {"unknown", 0}, {"error", 0}, {"invalid", 0}};
        int unresolved = 0;
        for (const auto& outcome : outcomes) {
            const auto& result = outcome.result;
            const auto status = hc::state_name(result.state);
            summary[status] = summary.value(status, 0) + 1;
            if (result.state != hc::State::Found && result.state != hc::State::NotFound && result.state != hc::State::Invalid)
                ++unresolved;
            auto entry = result_json(result);
            if (outcome.negative_control) entry["negative_control"] = result_json(*outcome.negative_control);
            results.push_back(entry);
            if (!options.json_stdout) {
                std::cout << std::left << std::setw(14) << status << std::setw(15) << result.site_name
                    << clean(result.reason) << '\n';
                if (result.state == hc::State::Found) std::cout << "  " << result.profile_url << '\n';
            }
        }
        json report = {{"tool", "handlecheck"}, {"version", "0.1.0"},
            {"mode", options.doctor ? "doctor" : "lookup"}, {"started_at", started_at},
            {"completed_at", timestamp()}, {"username", options.doctor ? json(nullptr) : json(options.username)},
            {"scope", "Public API profiles only. Shared handles do not establish a shared owner. not_found is limited to API visibility."},
            {"complete", unresolved == 0}, {"summary", summary}, {"results", results}};
        const std::string serialized = report.dump(2) + "\n";
        if (!options.output.empty()) {
            write_new_file(options.output, serialized);
            std::cerr << "Saved JSON report: " << options.output << '\n';
        }
        if (options.json_stdout) std::cout << serialized;
        else {
            std::cout << "\n" << summary["found"] << (options.doctor ? " healthy adapters, " : " confirmed, ")
                << summary["not_found"] << " no public record, " << summary["invalid"] << " invalid, "
                << unresolved << " unresolved.\n";
            if (unresolved) std::cout << "Scan incomplete: unresolved sites need review or a later retry.\n";
        }
        return unresolved ? 2 : 0;
    } catch (const std::exception& error) {
        if (http_ready) hc::http_global_cleanup();
        std::cerr << "handlecheck: " << clean(error.what()) << '\n';
        return 1;
    }
}
