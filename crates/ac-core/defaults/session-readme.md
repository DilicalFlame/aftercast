{{ channel.series }} #{{ stream.episode_padded }} - live coding, learning in public.
{% if quest is defined %}Today's quest: {{ quest.title }}{% endif %}

## What I'm doing today

<!-- private: write freely here during the stream; this whole section is yours -->
- 

## Chapters
{{ chapters_block }}
{% if questions_answered %}
## Questions answered
{% for q in questions_answered %}- {{ q.ts }} {{ q.author }}: {{ q.text }}
{% endfor %}{% endif %}
{% include "partials/links.md" %}

{% include "partials/support.md" %}

{{ vars.tagline }}
#coding #programming #livecoding
