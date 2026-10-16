import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';

// protocol-droid site. Industrial, monochrome, geometric.
// Logo is the brand; greyscale carries the page, signal amber for warnings only.
export default defineConfig({
  site: 'https://greysquirr3l.github.io',
  base: '/protocol-droid',
  integrations: [
    starlight({
      title: 'protocol-droid',
      description:
        'A Rust MCP toolkit that speaks every published spec revision at once.',
      logo: {
        src: './public/logo.svg',
        replacesTitle: false,
      },
      favicon: '/favicon.svg',
      customCss: ['./src/styles/brand.css', './src/styles/custom.css'],
      social: {
        github: 'https://github.com/greysquirr3l/protocol-droid',
      },
      // Force light mode on first paint. The theme toggle still works
      // (Starlight reads this attribute on switch); this just sets the
      // default for new visitors so the page doesn't flash dark.
      head: [
        {
          tag: 'script',
          content:
            "(function(){try{var t=localStorage.getItem('starlight-theme');if(t==='light'||t==='dark'){document.documentElement.dataset.theme=t;return}document.documentElement.dataset.theme='light';localStorage.setItem('starlight-theme','light')}catch(e){document.documentElement.dataset.theme='light'}})();",
        },
      ],
      sidebar: [
        {
          label: 'Getting started',
          autogenerate: { directory: 'getting-started' },
        },
        {
          label: 'Guides',
          autogenerate: { directory: 'guides' },
        },
        {
          label: 'Architecture',
          autogenerate: { directory: 'architecture' },
        },
        {
          label: 'Reference',
          autogenerate: { directory: 'reference' },
        },
        {
          label: 'Contributing',
          autogenerate: { directory: 'contributing' },
        },
      ],
    }),
  ],
});
