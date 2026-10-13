import 'dart:convert';

import 'package:http/http.dart' as http;
import 'package:nostr/nostr.dart' as nostr;

import '../../../shared/crypto/nip_oa.dart';
import '../../../shared/relay/relay.dart';

bool verifiedObserverEvent(NostrEvent event) {
  try {
    nostr.Event.fromJson(jsonEncode(event.toJson()));
    return true;
  } catch (_) {
    return false;
  }
}

String? _singleTag(NostrEvent event, String name) {
  final tags = event.tags.where((tag) => tag.isNotEmpty && tag.first == name);
  if (tags.length != 1 || tags.single.length != 2) return null;
  return tags.single[1];
}

NostrEvent? _latest(Iterable<NostrEvent> events) {
  final sorted = events.toList()
    ..sort((a, b) {
      final time = b.createdAt.compareTo(a.createdAt);
      return time != 0 ? time : a.id.compareTo(b.id);
    });
  return sorted.isEmpty ? null : sorted.first;
}

String? _profileOwner(NostrEvent? event) {
  if (event == null || event.kind != 0 || !verifiedObserverEvent(event)) {
    return null;
  }
  final tags = event.tags.where((tag) => tag.isNotEmpty && tag.first == 'auth');
  if (tags.length != 1 || tags.single.length != 4) return null;
  final owner = verifiedOaOwnerPubkey(event.tags, event.pubkey);
  if (owner == null) return null;
  final conditions = tags.single[2];
  for (final clause
      in conditions.isEmpty ? <String>[] : conditions.split('&')) {
    if (clause.startsWith('kind=') &&
        int.parse(clause.substring(5)) != event.kind) {
      return null;
    }
    if (clause.startsWith('created_at<') &&
        event.createdAt >= int.parse(clause.substring(11))) {
      return null;
    }
    if (clause.startsWith('created_at>') &&
        event.createdAt <= int.parse(clause.substring(11))) {
      return null;
    }
  }
  return owner;
}

/// Check current signed relay membership and the saved owner-signed agent policy.
/// No admission result is cached: removed viewers cannot receive future frames.
Future<bool> authorizeObserverChannel(
  RelaySessionNotifier session,
  RelayConfig config,
  String agent,
  String recipient,
  String channel,
) async {
  try {
    final response = await http
        .get(
          Uri.parse(config.baseUrl).resolve('/'),
          headers: {'Accept': 'application/nostr+json'},
        )
        .timeout(const Duration(seconds: 8));
    if (response.statusCode != 200) return false;
    final signer = (jsonDecode(response.body) as Map<String, dynamic>)['self'];
    if (signer is! String || !RegExp(r'^[0-9a-f]{64}$').hasMatch(signer)) {
      return false;
    }
    Future<NostrEvent?> head(int kind, String author) async => _latest(
      (await session.queryRelay([
        NostrFilter(
          kinds: [kind],
          authors: [author],
          tags: {
            '#d': [channel],
          },
          limit: 1,
        ),
      ])).where(
        (event) =>
            event.kind == kind &&
            event.pubkey == author &&
            event.tags.any(
              (tag) => tag.length >= 2 && tag.first == 'd' && tag[1] == channel,
            ),
      ),
    );
    final members = await head(39002, signer);
    if (members == null ||
        _singleTag(members, 'd') != channel ||
        !verifiedObserverEvent(members)) {
      return false;
    }
    final roster = members.tags
        .where((tag) => tag.length >= 2 && tag.first == 'p')
        .map((tag) => tag[1])
        .toSet();
    if (!roster.contains(agent) || !roster.contains(recipient)) return false;
    final profiles = await session.queryRelay([
      NostrFilter(kinds: [0], authors: [agent, recipient]),
    ]);
    final owner = _profileOwner(
      _latest(
        profiles.where((event) => event.pubkey == agent && event.kind == 0),
      ),
    );
    if (owner == null) return false;
    final policy = _latest(
      (await session.queryRelay([
        NostrFilter(
          kinds: [30177],
          authors: [owner],
          tags: {
            '#d': [agent],
          },
          limit: 1,
        ),
      ])).where(
        (event) =>
            event.pubkey == owner &&
            event.kind == 30177 &&
            event.tags.any(
              (tag) => tag.length >= 2 && tag.first == 'd' && tag[1] == agent,
            ),
      ),
    );
    if (policy == null ||
        _singleTag(policy, 'd') != agent ||
        !verifiedObserverEvent(policy)) {
      return false;
    }
    final body = jsonDecode(policy.content) as Map<String, dynamic>;
    if (body['name'] is! String ||
        body['parallelism'] is! int ||
        body['parallelism'] < 0 ||
        body['parallelism'] > 4294967295) {
      return false;
    }
    for (final field in [
      'persona_id',
      'system_prompt',
      'model',
      'provider',
      'persona_source_version',
    ]) {
      if (body[field] != null && body[field] is! String) return false;
    }
    final mode = body['respond_to'];
    if (!['owner-only', 'allowlist', 'anyone', 'nobody'].contains(mode)) {
      return false;
    }
    final allowlist = body.containsKey('respond_to_allowlist')
        ? body['respond_to_allowlist']
        : <dynamic>[];
    if (allowlist is! List ||
        allowlist.any(
          (key) =>
              key is! String || !RegExp(r'^[0-9a-fA-F]{64}$').hasMatch(key),
        )) {
      return false;
    }
    final metadata = await head(39000, signer);
    if (metadata == null ||
        _singleTag(metadata, 'd') != channel ||
        !verifiedObserverEvent(metadata)) {
      return false;
    }
    final isDm = metadata.tags.any(
      (tag) =>
          tag.isNotEmpty &&
          (tag.first == 'hidden' ||
              (tag.length >= 2 && tag.first == 't' && tag[1] == 'dm')),
    );
    final recipientOwner = _profileOwner(
      _latest(
        profiles.where((event) => event.pubkey == recipient && event.kind == 0),
      ),
    );
    final ownerOrSibling = recipient == owner || recipientOwner == owner;
    if (mode == 'nobody') return false;
    if (isDm || mode == 'owner-only') return ownerOrSibling;
    if (mode == 'anyone') return true;
    return ownerOrSibling ||
        allowlist.any((key) => (key as String).toLowerCase() == recipient);
  } catch (_) {
    return false;
  }
}
