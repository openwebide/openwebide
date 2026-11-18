const docsNavigation = document.querySelector('.docs-navigation');
if (docsNavigation) {
    const desktop = window.matchMedia('(min-width: 801px)');
    const setNavigation = () => { docsNavigation.open = desktop.matches; };
    desktop.addEventListener('change', setNavigation);
    setNavigation();
}
